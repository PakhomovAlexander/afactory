#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <time.h>
static volatile uint64_t sink = 1;
__attribute__((noinline)) static void leaf_work(uint64_t seed) {
  uint64_t value = seed;
  for (unsigned i = 0; i < 20000; ++i) value = value * 6364136223846793005ULL + 1442695040888963407ULL;
  sink ^= value;
}
__attribute__((noinline)) static void old_work(void) { leaf_work(sink); sink += 1; }
__attribute__((noinline)) static void new_work(void) { leaf_work(sink); sink += 3; }
__attribute__((noinline)) static void shared_work(void) { leaf_work(sink); sink += 7; }
static double seconds(void) { struct timespec now; clock_gettime(CLOCK_MONOTONIC, &now); return now.tv_sec + now.tv_nsec / 1e9; }
int main(int argc, char **argv) {
  int candidate = argc == 2 && strcmp(argv[1], "candidate") == 0;
  double until = seconds() + 10.0;
  unsigned iteration = 0;
  while (seconds() < until) {
    if ((iteration++ % 10) < (candidate ? 2 : 6)) { if (candidate) new_work(); else old_work(); }
    else shared_work();
  }
  printf("fixture-only result: %llu\n", (unsigned long long)sink);
  return 0;
}
