export default function (tokens, exec, ctx) {
  const kube = (ctx.environmentVariables && ctx.environmentVariables.KUBECONFIG) || "";
  const proc = ctx.currentProcess || "";
  return [
    {
      name: kube + "@" + proc,
      description: "kube context",
      icon: "fig://icon?type=kubernetes",
      priority: 75,
      type: "arg",
    },
  ];
}
