#!/bin/sh
set -eu
# Compile without writing to the reviewed source; only the private build root holds bytecode.
python3 -I -B -c 'import os, py_compile; py_compile.compile("fixture.py", cfile=os.environ["BUILD_ROOT"] + "/fixture.pyc", doraise=True)' >&2
python3 -I -B -c 'import sys, unittest; sys.path.insert(0,"."); result=unittest.TextTestRunner().run(unittest.defaultTestLoader.discover(".")); sys.exit(not result.wasSuccessful())' >&2
printf 'first command completed\n' > "$HOME/first-command"
read -r first < "$HOME/first-command"
test "$first" = 'first command completed'
printf '%s\n' '{"schema":"af.worker-reply/1","outputs":{"report":[{"summary":"Python fixture compiled and tested in a private reviewer experiment"}]}}'
