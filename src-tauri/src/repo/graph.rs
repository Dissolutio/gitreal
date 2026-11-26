use crate::model::{GraphEdge, GraphRow};

/// Assign lanes once for the complete topological snapshot. Holes are reused, never
/// compacted: moving another active lane would break the previous row's edges.
pub fn assign_lanes(rows: &mut [GraphRow]) {
    let mut lanes: Vec<Option<String>> = Vec::new();
    for row in rows {
        let lane = lanes
            .iter()
            .position(|id| id.as_ref() == Some(&row.commit_id))
            .unwrap_or_else(|| vacant_lane(&mut lanes));
        row.lane = lane;
        lanes[lane] = None;
        row.parent_edges = lanes
            .iter()
            .enumerate()
            .filter_map(|(index, id)| {
                id.as_ref().map(|id| GraphEdge {
                    from_lane: index,
                    to_lane: index,
                    parent_id: id.clone(),
                })
            })
            .collect();
        for parent in &row.parents {
            let target = lanes
                .iter()
                .position(|id| id.as_ref() == Some(parent))
                .unwrap_or_else(|| {
                    let index = vacant_lane(&mut lanes);
                    lanes[index] = Some(parent.clone());
                    index
                });
            row.parent_edges.push(GraphEdge {
                from_lane: lane,
                to_lane: target,
                parent_id: parent.clone(),
            });
        }
        while lanes.last() == Some(&None) {
            lanes.pop();
        }
    }
}

fn vacant_lane(lanes: &mut Vec<Option<String>>) -> usize {
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
    use std::collections::HashMap;

    fn check(history: &[(&str, &[&str])]) -> Vec<GraphRow> {
        let mut rows: Vec<_> = history
            .iter()
            .map(|(id, parents)| GraphRow {
                commit_id: id.to_string(),
                lane: 0,
                parent_edges: vec![],
                ref_labels: vec![],
                summary: String::new(),
                author: String::new(),
                date: 0,
                parents: parents.iter().map(|s| s.to_string()).collect(),
            })
            .collect();
        assign_lanes(&mut rows);
        let positions: HashMap<_, _> = rows
            .iter()
            .enumerate()
            .map(|(i, row)| (row.commit_id.as_str(), i))
            .collect();
        for (index, row) in rows.iter().enumerate() {
            for parent in &row.parents {
                let end = positions[parent.as_str()];
                assert!(end > index);
                let mut lane = row.lane;
                for step in &rows[index..end] {
                    let edge = step
                        .parent_edges
                        .iter()
                        .find(|edge| edge.from_lane == lane && edge.parent_id == *parent)
                        .expect("every parent connection continues through intermediate rows");
                    lane = edge.to_lane;
                }
                assert_eq!(lane, rows[end].lane);
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
    fn criss_cross_and_page_boundaries() {
        let rows = check(&[
            ("x", &["b", "c"]),
            ("y", &["c", "b"]),
            ("b", &["a"]),
            ("c", &["a"]),
            ("a", &[]),
        ]);
        for size in 1..=rows.len() {
            let pages: Vec<_> = rows.chunks(size).flatten().collect();
            for (original, paged) in rows.iter().zip(pages) {
                assert_eq!(original.lane, paged.lane);
                assert_eq!(original.parent_edges, paged.parent_edges);
            }
        }
    }

    #[test]
    fn fifty_thousand_linear_commits() {
        let mut rows: Vec<_> = (0..50_000)
            .map(|i| GraphRow {
                commit_id: i.to_string(),
                lane: 0,
                parent_edges: vec![],
                ref_labels: vec![],
                summary: String::new(),
                author: String::new(),
                date: 0,
                parents: if i == 49_999 {
                    vec![]
                } else {
                    vec![(i + 1).to_string()]
                },
            })
            .collect();
        assign_lanes(&mut rows);
        assert_eq!(rows.len(), 50_000);
        assert!(rows.iter().all(|row| row.lane == 0));
    }
}
