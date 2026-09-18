export default async function (tokens, exec) {
  const out = await exec({ command: "slow", args: ["job"] });
  return [{ name: out.stdout, type: "arg", priority: 50 }];
}
