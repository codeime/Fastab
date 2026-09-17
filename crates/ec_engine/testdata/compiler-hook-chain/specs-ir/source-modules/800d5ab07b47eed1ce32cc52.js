export default (function () {
"use strict";
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

const __ec_default_800d5ab07b47eed1 = ({
  name: "chain",
  args: [
    {
      name: "post",
      generators: { postProcess: makePostProcess("post") },
    },
    { name: "custom", generators: makeCustom("custom") },
  ],
});

return Object.freeze({
"chain#custom#1": (...__ec_args_800d5ab07b47eed1) => Reflect.apply(__ec_default_800d5ab07b47eed1["args"][1]["generators"]["custom"], __ec_default_800d5ab07b47eed1["args"][1]["generators"], __ec_args_800d5ab07b47eed1),
"chain#postProcess#0": __ec_default_800d5ab07b47eed1["args"][0]["generators"]["postProcess"]
});
})();
