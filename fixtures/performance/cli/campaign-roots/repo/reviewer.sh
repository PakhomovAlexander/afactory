#!/bin/sh
# The CLI Campaign fixture's reviewer (fixtures/performance/cli.toml, ADR-0169): one minor
# Finding for every line of src/fixture.txt, deterministic and model-free. Its input is read
# and ignored; it never reports a disposition or a demand.
cat >/dev/null
n=$(wc -l <src/fixture.txt | tr -d ' ')
printf '{"findings":['
i=1
while [ "$i" -le "$n" ]; do
    if [ "$i" -gt 1 ]; then
        printf ','
    fi
    printf '{"severity":"minor","file":"src/fixture.txt","line":%d,"title":"Fixture finding %d","body":"fixture","fix":"none","confidence":0.5}' "$i" "$i"
    i=$((i + 1))
done
printf '],"benchmark_demands":[],"dispositions":[]}'
