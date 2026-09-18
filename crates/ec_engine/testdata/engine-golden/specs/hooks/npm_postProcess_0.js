export default function (out) {
  return String(out)
    .split("\n")
    .filter(Boolean)
    .map((name) => ({
      name: name.trim(),
      description: "npm script",
      icon: "fig://icon?type=npm",
      priority: 60,
      type: "arg",
      shouldAddSpace: true,
    }));
}
