export default function (tokens, exec, ctx) {
  const home = (ctx.environmentVariables && ctx.environmentVariables.HOME) || "";
  const proc = ctx.currentProcess || "";
  return [
    {
      name: home + ":" + proc,
      description: "env context",
      priority: 80,
      type: "arg",
    },
  ];
}
