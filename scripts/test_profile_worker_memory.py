#!/usr/bin/env python3
"""Focused observer boundary tests; synthetic /proc/cgroup, no Docker/model."""

import pathlib
import runpy
import subprocess
import tempfile
import unittest


COLLECTOR = runpy.run_path(str(pathlib.Path(__file__).with_name("profile-worker-memory.py")))
STATUS = """VmPeak: 123 kB
VmHWM: 100 kB
VmRSS: 80 kB
RssAnon: 60 kB
RssFile: 20 kB
Threads: 2
VmSwap: 0 kB
"""


class ProbeBoundaryTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="ocr-memory-probe-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = pathlib.Path(self.temporary.name)
        self.proc = self.root / "proc"
        self.cgroup = self.root / "cgroup"
        self.proc.mkdir()
        self.cgroup.mkdir()
        (self.proc / "uptime").write_text("12.34 0.00\n")
        for name, content in {
            "memory.current": "1000\n", "memory.peak": "1200\n", "memory.max": "2000\n",
            "memory.swap.current": "0\n", "memory.events": "low 0\nhigh 0\nmax 0\noom 0\noom_kill 0\n",
            "memory.events.local": "low 0\nhigh 0\nmax 0\noom 0\noom_kill 0\n",
            "memory.stat": "anon 800\nfile 100\nkernel 100\nshmem 0\n",
            "cpu.stat": "usage_usec 3\nuser_usec 2\nsystem_usec 1\n",
            "memory.pressure": "some avg10=0.00 avg60=0.00 avg300=0.00 total=10\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=0\n",
        }.items():
            (self.cgroup / name).write_text(content)
        self.add_process("1", "ocr-worker", STATUS)

    def add_process(self, number, name, status):
        directory = self.proc / number
        directory.mkdir()
        (directory / "comm").write_text(name + "\n")
        if status is not None:
            (directory / "status").write_text(status)

    def probe(self):
        # Execute the real shell loop, only remapping its filesystem roots to
        # synthetic files. One sample needs no clock progression or sleeping.
        source = COLLECTOR["PROBE"].replace("/sys/fs/cgroup/", str(self.cgroup) + "/")
        source = source.replace("/proc/", str(self.proc) + "/")
        return subprocess.run(["sh", "-s", "--", "200", "0.200", "1"], input=source,
                              text=True, capture_output=True, timeout=3)

    def test_optional_process_disappears_after_comm(self):
        self.add_process("2", "pdftoppm", None)
        result = self.probe()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")
        sample, = COLLECTOR["parse_probe"](result.stdout)
        self.assertEqual(sample["observer"]["optional_processes_unavailable"], 1)
        self.assertEqual([p["name"] for p in sample["processes"]], ["ocr-worker"])

    def test_terminal_optional_process_lacks_memory_fields(self):
        self.add_process("2", "pdfinfo", "Name:\tpdfinfo\nState:\tZ (zombie)\nThreads:\t1\n")
        result = self.probe()
        self.assertEqual(result.returncode, 0, result.stderr)
        sample, = COLLECTOR["parse_probe"](result.stdout)
        self.assertEqual(sample["observer"]["optional_processes_unavailable"], 1)
        self.assertEqual(len(sample["processes"]), 1)

    def test_unidentified_process_disappears_before_comm_read(self):
        (self.proc / "2").mkdir()
        result = self.probe()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")
        sample, = COLLECTOR["parse_probe"](result.stdout)
        self.assertEqual(sample["observer"]["optional_processes_unavailable"], 0)
        self.assertEqual(len(sample["processes"]), 1)

    def test_missing_required_worker_status_fails(self):
        (self.proc / "1" / "status").unlink()
        self.assertNotEqual(self.probe().returncode, 0)

    def test_missing_required_worker_fields_fail(self):
        (self.proc / "1" / "status").write_text("Threads:\t1\n")
        self.assertNotEqual(self.probe().returncode, 0)

    def test_missing_required_cgroup_metric_fails(self):
        (self.cgroup / "memory.current").unlink()
        self.assertNotEqual(self.probe().returncode, 0)

    def test_parser_requires_observer_counter_and_worker(self):
        result = self.probe()
        self.assertEqual(result.returncode, 0, result.stderr)
        COLLECTOR["parse_probe"](result.stdout)
        without_counter = "\n".join(line for line in result.stdout.splitlines()
                                    if not line.startswith("OBSERVER\t"))
        without_worker = "\n".join(line for line in result.stdout.splitlines()
                                   if not line.startswith("PROC\tocr-worker\t"))
        for invalid in (without_counter, without_worker):
            with self.assertRaises(RuntimeError):
                COLLECTOR["parse_probe"](invalid)


if __name__ == "__main__":
    unittest.main()
