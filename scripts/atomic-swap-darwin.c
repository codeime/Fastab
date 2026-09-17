#include <stdio.h>
#include <string.h>

int main(int argc, char *argv[]) {
    if (argc != 4) {
        fputs("usage: atomic-swap-darwin <swap|exclusive> <source> <destination>\n", stderr);
        return 64;
    }
    unsigned int flags;
    const char *operation;
    if (strcmp(argv[1], "swap") == 0) {
        flags = RENAME_SWAP;
        operation = "renamex_np(RENAME_SWAP)";
    } else if (strcmp(argv[1], "exclusive") == 0) {
        flags = RENAME_EXCL;
        operation = "renamex_np(RENAME_EXCL)";
    } else {
        fputs("atomic-swap-darwin: unknown operation\n", stderr);
        return 64;
    }
    if (renamex_np(argv[2], argv[3], flags) != 0) {
        perror(operation);
        return 1;
    }
    return 0;
}
