export default function (out) {
  return String(out)
    .split("\n")
    .filter(Boolean)
    .map((name) => ({
      name: name.trim(),
      description: "git branch",
      icon: "fig://icon?type=git",
      priority: 70,
      type: "arg",
      shouldAddSpace: true,
    }));
}
