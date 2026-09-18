export default function (out) {
  return String(out)
    .split("\n")
    .filter(Boolean)
    .map((name) => ({
      name: name.trim(),
      description: "api resource",
      icon: "fig://icon?type=kubernetes",
      priority: 55,
      type: "arg",
      shouldAddSpace: true,
    }));
}
