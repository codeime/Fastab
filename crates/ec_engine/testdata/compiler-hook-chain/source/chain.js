const prefix = "compiled";

const makePostProcess = (label) => (stdout) => [
  { name: `${prefix}:${label}:${stdout}` },
];

const makeCustom = (label) => ({
  label,
  custom(tokens, executeCommand, context) {
    return [
      {
        name: [
          prefix,
          this.label,
          tokens[0],
          typeof executeCommand,
          context.searchTerm,
          context.currentWorkingDirectory,
          context.currentProcess,
          context.environmentVariables.EC_HOOK,
        ].join(":"),
      },
    ];
  },
});

export default {
  name: "chain",
  args: [
    {
      name: "post",
      generators: { postProcess: makePostProcess("post") },
    },
    { name: "custom", generators: makeCustom("custom") },
  ],
};
