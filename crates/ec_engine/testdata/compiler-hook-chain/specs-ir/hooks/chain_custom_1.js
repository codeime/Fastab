export default function(tokens, executeCommand, context) {
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
  };
