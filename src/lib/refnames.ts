export function validateBranchName(name: string): string | null {
  if (!name || name.startsWith("-") || /[\s\0-\x1f\x7f]/.test(name)) {
    return "Enter a valid branch name.";
  }
  const components = name.split("/");
  if (
    name === "@" ||
    name.endsWith("/") ||
    name.endsWith(".") ||
    name.includes("..") ||
    name.includes("@{") ||
    /[~^:?*\\]/.test(name) ||
    components.some(part => !part || part.startsWith(".") || part.endsWith(".") || part.endsWith(".lock"))
  ) {
    return "Enter a valid branch name.";
  }
  return null;
}
