export default async function (tokens, exec, ctx) {
  const out = await exec({
    command: "cache-source",
    args: [ctx.currentWorkingDirectory],
  });
  return String(out.stdout)
    .split("\n")
    .filter(Boolean)
    .map((name) => ({
      name,
      description: "cached",
      priority: 60,
      type: "arg",
    }));
}
