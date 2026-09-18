export default function (out) {
  return String(out)
    .split("\n")
    .filter(Boolean)
    .map((name) => ({
      name: name.trim(),
      description: "docker image",
      icon: "fig://icon?type=docker",
      priority: 65,
      type: "arg",
      shouldAddSpace: true,
    }));
}
