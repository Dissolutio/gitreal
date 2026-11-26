use crate::model::GraphEdge;
use std::collections::HashMap;
use std::sync::Arc;

const CHECKPOINT_INTERVAL: usize = 256;

#[derive(Debug)]
pub(crate) struct LaneCheckpoint {
    pub row: usize,
    pub lanes: Vec<Option<u32>>,
}

#[derive(Debug)]
pub(crate) struct LaneLayout {
    pub lanes: Vec<u32>,
    pub checkpoints: Vec<LaneCheckpoint>,
    pub max_lane: usize,
}

pub(crate) fn assign_lanes(parents: &[Vec<u32>]) -> LaneLayout {
    let mut lanes: Vec<Option<u32>> = Vec::new();
    let mut assigned = Vec::with_capacity(parents.len());
    let mut checkpoints = Vec::new();
    let mut max_lane = 0;

    for (row_index, row_parents) in parents.iter().enumerate() {
        if row_index % CHECKPOINT_INTERVAL == 0 {
            checkpoints.push(LaneCheckpoint {
                row: row_index,
                lanes: lanes.clone(),
            });
        }

        let row_index = row_index as u32;
        let lane = lanes
            .iter()
            .position(|id| *id == Some(row_index))
            .unwrap_or_else(|| vacant_lane(&mut lanes));
        assigned.push(lane as u32);
        max_lane = max_lane.max(lane);
        lanes[lane] = None;

        for (parent_index, parent) in row_parents.iter().copied().enumerate() {
            let existing = lanes.iter().position(|id| *id == Some(parent));
            let target = match (parent_index, existing) {
                (_, Some(target)) => target,
                (0, None) => {
                    lanes[lane] = Some(parent);
                    lane
                }
                (_, None) => {
                    let target = lanes
                        .iter()
                        .enumerate()
                        .skip(lane + 1)
                        .find_map(|(index, id)| id.is_none().then_some(index))
                        .unwrap_or_else(|| {
                            lanes.push(None);
                            lanes.len() - 1
                        });
                    lanes[target] = Some(parent);
                    target
                }
            };
            max_lane = max_lane.max(target);
        }

        while lanes.last() == Some(&None) {
            lanes.pop();
        }
    }

    LaneLayout {
        lanes: assigned,
        checkpoints,
        max_lane,
    }
}

pub(crate) fn page_edges(
    parents: &[Vec<u32>],
    layout: &LaneLayout,
    start: usize,
    end: usize,
    id_for_row: impl Fn(u32) -> Arc<str>,
) -> Vec<Vec<GraphEdge>> {
    if start >= end {
        return Vec::new();
    }

    let checkpoint = layout
        .checkpoints
        .get(start / CHECKPOINT_INTERVAL)
        .expect("every non-empty page has an earlier lane checkpoint");
    let mut lanes = checkpoint.lanes.clone();
    let mut lane_by_row: HashMap<_, _> = lanes
        .iter()
        .enumerate()
        .filter_map(|(lane, row)| row.map(|row| (row, lane)))
        .collect();
    let mut lane_ids: Vec<Option<Arc<str>>> =
        lanes.iter().map(|row| row.map(&id_for_row)).collect();
    let mut page = Vec::with_capacity(end - start);

    for row_index in checkpoint.row..end {
        let row_index = row_index as u32;
        let lane = lane_by_row
            .remove(&row_index)
            .unwrap_or_else(|| vacant_lane(&mut lanes));
        debug_assert_eq!(layout.lanes[row_index as usize], lane as u32);
        if lane_ids.len() < lanes.len() {
            lane_ids.resize_with(lanes.len(), || None);
        }
        lanes[lane] = None;
        lane_ids[lane] = None;

        let include_edges = row_index as usize >= start;
        let mut edges = if include_edges {
            Vec::with_capacity(
                lanes.iter().filter(|id| id.is_some()).count() + parents[row_index as usize].len(),
            )
        } else {
            Vec::new()
        };
        if include_edges {
            edges.extend(lanes.iter().enumerate().filter_map(|(index, id)| {
                id.map(|_| GraphEdge {
                    from_lane: index,
                    to_lane: index,
                    parent_id: lane_ids[index].as_ref().unwrap().clone(),
                })
            }));
        }

        for (parent_index, parent) in parents[row_index as usize].iter().copied().enumerate() {
            let existing = lane_by_row.get(&parent).copied();
            let target = match (parent_index, existing) {
                (_, Some(target)) => target,
                (0, None) => {
                    lanes[lane] = Some(parent);
                    lane_by_row.insert(parent, lane);
                    lane_ids[lane] = Some(id_for_row(parent));
                    lane
                }
                (_, None) => {
                    let target = lanes
                        .iter()
                        .enumerate()
                        .skip(lane + 1)
                        .find_map(|(index, id)| id.is_none().then_some(index))
                        .unwrap_or_else(|| {
                            lanes.push(None);
                            lanes.len() - 1
                        });
                    lanes[target] = Some(parent);
                    lane_by_row.insert(parent, target);
                    if lane_ids.len() < lanes.len() {
                        lane_ids.resize_with(lanes.len(), || None);
                    }
                    lane_ids[target] = Some(id_for_row(parent));
                    target
                }
            };
            if include_edges {
                edges.push(GraphEdge {
                    from_lane: lane,
                    to_lane: target,
                    parent_id: lane_ids[target].as_ref().unwrap().clone(),
                });
            }
        }

        while lanes.last() == Some(&None) {
            lanes.pop();
            lane_ids.pop();
        }
        if include_edges {
            page.push(edges);
        }
    }

    page
}

fn vacant_lane(lanes: &mut Vec<Option<u32>>) -> usize {
    if let Some(index) = lanes.iter().position(Option::is_none) {
        index
    } else {
        lanes.push(None);
        lanes.len() - 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::GraphRow;
    use gix::ObjectId;
    use std::collections::HashMap;

    fn check(history: &[(&str, &[&str])]) -> Vec<GraphRow> {
        let positions: HashMap<_, _> = history
            .iter()
            .enumerate()
            .map(|(index, (id, _))| (*id, index as u32))
            .collect();
        let parents: Vec<Vec<_>> = history
            .iter()
            .map(|(_, parents)| parents.iter().map(|id| positions[id]).collect())
            .collect();
        let ids: Vec<_> = (0..history.len())
            .map(|index| ObjectId::from_hex(format!("{:040x}", index + 1).as_bytes()).unwrap())
            .collect();
        let layout = assign_lanes(&parents);
        let edges = page_edges(&parents, &layout, 0, history.len(), |index| {
            Arc::from(ids[index as usize].to_string())
        });
        let rows: Vec<_> = history
            .iter()
            .enumerate()
            .map(|(index, (id, parents))| GraphRow {
                commit_id: (*id).into(),
                lane: layout.lanes[index] as usize,
                parent_edges: edges[index].clone(),
                ref_labels: vec![],
                summary: String::new(),
                author: String::new(),
                date: 0,
                parents: parents.iter().map(|id| (*id).into()).collect(),
            })
            .collect();

        for (index, row) in rows.iter().enumerate() {
            for parent in &row.parents {
                let parent_index = positions[parent.as_str()] as usize;
                assert!(parent_index > index);
                let mut lane = row.lane;
                let expected_parent = ids[parent_index].to_string();
                for step in &rows[index..parent_index] {
                    let edge = step
                        .parent_edges
                        .iter()
                        .find(|edge| {
                            edge.from_lane == lane
                                && edge.parent_id.as_ref() == expected_parent.as_str()
                        })
                        .expect("every parent connection continues through intermediate rows");
                    lane = edge.to_lane;
                }
                assert_eq!(lane, rows[parent_index].lane);
            }
        }
        rows
    }

    #[test]
    fn linear() {
        assert!(
            check(&[("c", &["b"]), ("b", &["a"]), ("a", &[])])
                .iter()
                .all(|r| r.lane == 0)
        );
    }

    #[test]
    fn branch() {
        check(&[("b", &["a"]), ("c", &["a"]), ("a", &[])]);
    }

    #[test]
    fn merge() {
        check(&[("m", &["b", "c"]), ("b", &["a"]), ("c", &["a"]), ("a", &[])]);
    }

    #[test]
    fn octopus() {
        check(&[
            ("m", &["b", "c", "d"]),
            ("b", &["a"]),
            ("c", &["a"]),
            ("d", &["a"]),
            ("a", &[]),
        ]);
    }

    #[test]
    fn multiple_roots() {
        check(&[("b", &["a"]), ("d", &["c"]), ("a", &[]), ("c", &[])]);
    }

    #[test]
    fn criss_cross_connectivity() {
        check(&[
            ("x", &["b", "c"]),
            ("y", &["c", "b"]),
            ("b", &["a"]),
            ("c", &["a"]),
            ("a", &[]),
        ]);
    }

    #[test]
    fn feature_branch_and_main_first_parent_stay_in_their_lanes() {
        let rows = check(&[
            ("merge", &["main-tip", "feature-2"]),
            ("main-tip", &["main-mid"]),
            ("main-mid", &["fork"]),
            ("feature-2", &["feature-1"]),
            ("feature-1", &["fork"]),
            ("fork", &[]),
        ]);
        assert_eq!(rows[3].lane, rows[4].lane);
        assert_ne!(rows[3].lane, rows[1].lane);
        assert!(rows[..3].iter().all(|row| row.lane == 0));
        assert_eq!(rows[5].lane, 0);
    }

    #[test]
    fn newly_assigned_first_parent_keeps_the_commit_lane() {
        let rows = check(&[
            ("merge", &["main", "feature"]),
            ("main", &["fork"]),
            ("feature", &["feature-parent"]),
            ("feature-parent", &["fork"]),
            ("fork", &[]),
        ]);
        assert_eq!(rows[2].lane, rows[3].lane);
        assert_eq!(rows[2].lane, 1);
    }

    #[test]
    fn page_edges_match_full_history_for_every_page_size() {
        let row_count = 600;
        let parents: Vec<Vec<_>> = (0..row_count)
            .map(|index| match index {
                0 => vec![1, 300],
                1..=298 => vec![(index + 1) as u32],
                299 => vec![500],
                300..=498 => vec![(index + 1) as u32],
                499 => vec![500],
                500 => vec![501],
                501..=598 => vec![(index + 1) as u32],
                _ => Vec::new(),
            })
            .collect();
        let ids: Vec<_> = (0..row_count)
            .map(|index| ObjectId::from_hex(format!("{:040x}", index + 1).as_bytes()).unwrap())
            .collect();
        let layout = assign_lanes(&parents);
        let all = page_edges(&parents, &layout, 0, row_count, |index| {
            Arc::from(ids[index as usize].to_string())
        });
        for size in 1..=row_count {
            let paged: Vec<_> = (0..row_count)
                .step_by(size)
                .flat_map(|start| {
                    page_edges(
                        &parents,
                        &layout,
                        start,
                        (start + size).min(row_count),
                        |index| Arc::from(ids[index as usize].to_string()),
                    )
                })
                .collect();
            assert_eq!(all, paged);
        }
    }

    #[test]
    fn fifty_thousand_linear_commits() {
        let parents: Vec<Vec<_>> = (0..50_000)
            .map(|index| {
                if index + 1 == 50_000 {
                    vec![]
                } else {
                    vec![(index + 1) as u32]
                }
            })
            .collect();
        let layout = assign_lanes(&parents);
        assert_eq!(layout.lanes.len(), 50_000);
        assert!(layout.lanes.iter().all(|lane| *lane == 0));
    }
}
