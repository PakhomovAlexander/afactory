#!/usr/bin/env python3
"""Run an existing script unchanged, recording real unittest cases when it uses unittest.
Scripts without unittest retain only ci-step evidence; no synthetic passed test is invented.
"""
from pathlib import Path
import runpy
import sys
import time
import unittest
import xml.etree.ElementTree as ET


def main():
    script, output = sys.argv[1:]
    Path(output).parent.mkdir(parents=True, exist_ok=True)
    Path(output).unlink(missing_ok=True)
    records = []
    started = time.monotonic()
    original = unittest.TextTestRunner

    class Result(unittest.TextTestResult):
        def startTest(self, test):
            self.clock = time.monotonic()
            self.record = {'name': test.id(), 'classname': script}
            self.outcome = None
            super().startTest(test)

        def addFailure(self, test, err):
            self.outcome = 'failure'
            super().addFailure(test, err)

        def addError(self, test, err):
            self.outcome = 'error'
            super().addError(test, err)

        def addSkip(self, test, reason):
            self.outcome = 'skipped'
            super().addSkip(test, reason)

        def addExpectedFailure(self, test, err):
            self.outcome = 'skipped'
            super().addExpectedFailure(test, err)

        def addUnexpectedSuccess(self, test):
            self.outcome = 'failure'
            super().addUnexpectedSuccess(test)

        def addSubTest(self, test, subtest, err):
            if err is not None:
                self.outcome = 'failure'
            super().addSubTest(test, subtest, err)

        def stopTest(self, test):
            self.record['time'] = str(time.monotonic() - self.clock)
            records.append((self.record, self.outcome))
            super().stopTest(test)

    class Runner(original):
        resultclass = Result

    unittest.TextTestRunner = Runner
    unittest.runner.TextTestRunner = Runner
    sys.argv = [str(Path(__file__).with_name(script))]
    try:
        runpy.run_path(sys.argv[0], run_name='__main__')
    finally:
        if records:
            root = ET.Element('testsuites', time=str(time.monotonic() - started), tests=str(len(records)))
            suite = ET.SubElement(root, 'testsuite', name=script)
            for attributes, outcome in records:
                case = ET.SubElement(suite, 'testcase', attributes)
                if outcome:
                    ET.SubElement(case, outcome)
            ET.ElementTree(root).write(output, encoding='unicode')


if __name__ == '__main__':
    main()
