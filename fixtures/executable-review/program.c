#include <stdio.h>
#include <string.h>
int main(int argc, char **argv) {
    if (argc != 2 || strcmp(argv[1], "exact") != 0) return 1;
    puts("exact candidate executed");
    return 0;
}
