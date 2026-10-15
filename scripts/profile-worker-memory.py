#!/usr/bin/env python3
"""Read-only bounded OCR worker/cgroup and separate host memory evidence.

Requires an explicit container name. Does not inspect arguments/environment,
submit jobs, start containers, reset peaks, change limits, or load models.
"""

import argparse
import datetime
import json
import math
import os
import pathlib
import re
import subprocess
import sys
import threading
import time


SCHEMA = "ocr-memory-profile/1.0.0"
# One docker exec owns this finite loop; disconnecting the client cannot leave
# an unbounded observer in the container. Only fixed, non-content fields leave it.
PROBE = r'''
set -eu
export LC_ALL=C
duration_ms=$1
interval=$2
max_samples=$3
uptime_ms() {
    read -r uptime unused < /proc/uptime
    whole=${uptime%.*}
    fraction=${uptime#*.}
    # Linux /proc/uptime is expressed with two decimal places.
    now_ms=$((whole * 1000 + (1$fraction - 100) * 10))
}
metric() {
    test -r "$2" || { echo 'required cgroup metric unavailable' >&2; exit 2; }
    read -r value < "$2"
    printf 'M\t%s\t%s\n' "$1" "$value"
}
uptime_ms
end_ms=$((now_ms + duration_ms))
sample=0
while test "$sample" -lt "$max_samples"; do
    uptime_ms
    test "$sample" -eq 0 || test "$now_ms" -le "$end_ms" || break
    printf 'BEGIN\t%s\t%s\n' "$sample" "$now_ms"
    metric current /sys/fs/cgroup/memory.current
    metric peak /sys/fs/cgroup/memory.peak
    metric max /sys/fs/cgroup/memory.max
    metric swap_current /sys/fs/cgroup/memory.swap.current
    for collection in memory.events memory.events.local memory.stat cpu.stat; do
        test -r "/sys/fs/cgroup/$collection" || { echo 'required cgroup collection unavailable' >&2; exit 2; }
        while read -r key value unused; do
            case "$collection:$key" in
                memory.events:low|memory.events:high|memory.events:max|memory.events:oom|memory.events:oom_kill|memory.events:oom_group_kill|memory.events.local:low|memory.events.local:high|memory.events.local:max|memory.events.local:oom|memory.events.local:oom_kill|memory.events.local:oom_group_kill|memory.stat:anon|memory.stat:file|memory.stat:kernel|memory.stat:shmem|cpu.stat:usage_usec|cpu.stat:user_usec|cpu.stat:system_usec|cpu.stat:nr_throttled|cpu.stat:throttled_usec)
                    printf 'C\t%s\t%s\t%s\n' "$collection" "$key" "$value" ;;
            esac
        done < "/sys/fs/cgroup/$collection"
    done
    test -r /sys/fs/cgroup/memory.pressure || { echo 'memory pressure unavailable' >&2; exit 2; }
    while read -r category avg10 avg60 avg300 total; do
        printf 'PRESSURE\t%s\t%s\t%s\t%s\t%s\n' "$category" "$avg10" "$avg60" "$avg300" "$total"
    done < /sys/fs/cgroup/memory.pressure
    scanned=0
    matched=0
    optional_unavailable=0
    for directory in /proc/[0-9]*; do
        scanned=$((scanned + 1))
        test "$scanned" -le 512 || { echo 'process scan cap exceeded' >&2; exit 2; }
        # A process can disappear even between a readability check and open.
        # Until its name is known it is outside the observed process set; the
        # parser still requires a complete worker row in every sample.
        if ! name=$(cat "$directory/comm" 2>/dev/null); then
            continue
        fi
        case "$name" in ocr-worker|layout-detect|pdftoppm|pdfinfo|pdftotext) ;; *) continue ;; esac
        matched=$((matched + 1))
        test "$matched" -le 32 || { echo 'observed process cap exceeded' >&2; exit 2; }
        # Poppler and recyclable layout helpers can disappear after comm was
        # observed. Capture status under an explicit conditional so a normal
        # /proc race does not trigger set -e or leak an unframed shell error.
        if process_status=$(cat "$directory/status" 2>/dev/null); then
            :
        elif test "$name" = ocr-worker; then
            echo 'required worker status unavailable' >&2
            exit 2
        else
            optional_unavailable=$((optional_unavailable + 1))
            continue
        fi
        peak=; hwm=; rss=; anon=; file=; threads=; swap=
        while read -r key value unused; do
            case "$key" in
                VmPeak:) peak=$value ;; VmHWM:) hwm=$value ;;
                VmRSS:) rss=$value ;; RssAnon:) anon=$value ;;
                RssFile:) file=$value ;; Threads:) threads=$value ;;
                VmSwap:) swap=$value ;;
            esac
        done <<PROCESS_STATUS
$process_status
PROCESS_STATUS
        # A helper can reach terminal/zombie state without memory fields. Count
        # that unavailable observation, never invent a zero-RSS process row.
        if test -n "$peak" && test -n "$hwm" && test -n "$rss" && test -n "$anon" && test -n "$file" && test -n "$threads" && test -n "$swap"; then
            :
        elif test "$name" = ocr-worker; then
            echo 'required worker memory fields unavailable' >&2
            exit 2
        else
            optional_unavailable=$((optional_unavailable + 1))
            continue
        fi
        printf 'PROC\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$name" "$peak" "$hwm" "$rss" "$anon" "$file" "$threads" "$swap"
    done
    printf 'OBSERVER\toptional_processes_unavailable\t%s\n' "$optional_unavailable"
    printf 'END\n'
    sample=$((sample + 1))
    test "$sample" -ge "$max_samples" || sleep "$interval"
done
'''


def utc_now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def positive_pid(value):
    if not re.fullmatch(r"[1-9][0-9]{0,9}", value):
        raise argparse.ArgumentTypeError("PID must be a positive decimal integer")
    return int(value)


def fixed_command(args):
    result = subprocess.run(args, capture_output=True, text=True, timeout=5, check=False)
    if result.returncode or len(result.stdout) > 65536:
        raise RuntimeError("host observation command failed or exceeded its output bound")
    return result.stdout


def host_snapshot(native_pid, vm_pid):
    record = {"schema_version": SCHEMA, "kind": "host_snapshot", "observed_at": utc_now(),
              "monotonic_seconds": time.monotonic(), "platform": sys.platform,
              "overlap_warning": "Host, Docker VM, cgroup and unified GPU memory overlap; do not sum them.",
              "native_model_runtime_allocated_bytes": None,
              "native_model_runtime_peak_bytes": None}
    if sys.platform == "darwin":
        record["physical_bytes"] = int(fixed_command(["sysctl", "-n", "hw.memsize"]).strip())
        swap = fixed_command(["sysctl", "vm.swapusage"])
        values = {}
        for key, value, unit in re.findall(r"(total|used|free)\s*=\s*([0-9.]+)([KMG])", swap):
            values[key + "_bytes"] = round(float(value) * {"K": 1024, "M": 1024**2, "G": 1024**3}[unit])
        if len(values) != 3:
            raise RuntimeError("required host swap metrics unavailable")
        record["swap"] = values
        vm_stat = fixed_command(["vm_stat"])
        page = re.search(r"page size of (\d+) bytes", vm_stat)
        if page is None:
            raise RuntimeError("host VM page size unavailable")
        record["vm"] = {"page_size_bytes": int(page.group(1)), "counters": {}}
        for label, number in re.findall(r"^([^:\n]+):\s+(\d+)\.?\s*$", vm_stat, re.MULTILINE):
            record["vm"]["counters"][label] = int(number)
        if "Swapins" not in record["vm"]["counters"] or "Swapouts" not in record["vm"]["counters"]:
            raise RuntimeError("host swap counters unavailable")
        pressure = fixed_command(["memory_pressure", "-Q"])
        free = re.search(r"System-wide memory free percentage:\s*(\d+)%", pressure)
        if free is None:
            raise RuntimeError("host memory pressure observation unavailable")
        record["system_memory_free_percent"] = int(free.group(1))
    elif sys.platform.startswith("linux"):
        meminfo = {}
        for line in pathlib.Path("/proc/meminfo").read_text().splitlines():
            fields = line.split()
            if len(fields) == 3 and fields[2] == "kB":
                meminfo[fields[0].rstrip(":")] = int(fields[1]) * 1024
        for key in ("MemTotal", "MemAvailable", "SwapTotal", "SwapFree"):
            if key not in meminfo:
                raise RuntimeError("required host meminfo fields unavailable")
        record["physical_bytes"] = meminfo["MemTotal"]
        record["available_bytes"] = meminfo["MemAvailable"]
        record["swap"] = {"total_bytes": meminfo["SwapTotal"], "free_bytes": meminfo["SwapFree"],
                          "used_bytes": meminfo["SwapTotal"] - meminfo["SwapFree"]}
        record["memory_pressure"] = pathlib.Path("/proc/pressure/memory").read_text().strip()
    else:
        raise RuntimeError("host collector supports macOS and Linux")
    for role, pid in (("native_model", native_pid), ("docker_vm", vm_pid)):
        if pid is None:
            record[role] = {"rss_bytes": None, "physical_footprint_bytes": None,
                            "status": "not_requested; pass an explicit PID to observe RSS"}
            continue
        output = fixed_command(["ps", "-p", str(pid), "-o", "rss="]).strip()
        if not output.isdecimal():
            raise RuntimeError("explicit host process RSS unavailable")
        record[role] = {"rss_bytes": int(output) * 1024, "physical_footprint_bytes": None,
                        "status": "RSS only; physical footprint and unified GPU allocation are unmeasured"}
    return record


def integer(value):
    if not value.isdecimal():
        raise RuntimeError("non-numeric required container metric")
    return int(value)


def parse_probe(output):
    records = []
    record = None
    for line in output.splitlines():
        fields = line.split("\t")
        tag = fields[0]
        if tag == "BEGIN" and len(fields) == 3 and record is None:
            record = {"schema_version": SCHEMA, "kind": "worker_memory_sample",
                      "sample_index": integer(fields[1]), "container_uptime_ms": integer(fields[2]),
                      "cgroup": {}, "collections": {}, "pressure": {}, "processes": []}
        elif record is None:
            raise RuntimeError("invalid container sample framing")
        elif tag == "M" and len(fields) == 3:
            record["cgroup"][fields[1]] = (None if fields[1] == "max" and fields[2] == "max"
                                           else integer(fields[2]))
        elif tag == "C" and len(fields) == 4:
            record["collections"].setdefault(fields[1], {})[fields[2]] = integer(fields[3])
        elif tag == "PRESSURE" and len(fields) == 6:
            values = {}
            for field in fields[2:]:
                key, value = field.split("=", 1)
                number = float(value)
                if not math.isfinite(number) or number < 0:
                    raise RuntimeError("invalid memory pressure value")
                values[key] = integer(value) if key == "total" else number
            if set(values) != {"avg10", "avg60", "avg300", "total"}:
                raise RuntimeError("memory pressure fields missing")
            record["pressure"][fields[1]] = values
        elif tag == "PROC" and len(fields) == 9:
            names = ("virtual_peak_bytes", "rss_peak_bytes", "rss_bytes", "rss_anon_bytes",
                     "rss_file_bytes", "threads", "swap_bytes")
            process = {"name": fields[1]}
            for key, value in zip(names, fields[2:]):
                process[key] = integer(value) * (1 if key == "threads" else 1024)
            record["processes"].append(process)
        elif tag == "OBSERVER" and len(fields) == 3 and fields[1] == "optional_processes_unavailable":
            if "observer" in record:
                raise RuntimeError("duplicate observer counters")
            record["observer"] = {fields[1]: integer(fields[2])}
        elif tag == "END" and len(fields) == 1:
            required = {"memory.events": {"low", "high", "max", "oom", "oom_kill"},
                        "memory.events.local": {"low", "high", "max", "oom", "oom_kill"},
                        "memory.stat": {"anon", "file", "kernel", "shmem"},
                        "cpu.stat": {"usage_usec", "user_usec", "system_usec"}}
            if set(record["cgroup"]) != {"current", "peak", "max", "swap_current"}:
                raise RuntimeError("required cgroup memory metrics missing")
            for collection, keys in required.items():
                if not keys <= record["collections"].get(collection, {}).keys():
                    raise RuntimeError("required cgroup collection fields missing")
            if set(record["pressure"]) != {"some", "full"}:
                raise RuntimeError("required cgroup pressure metrics missing")
            if not any(process["name"] == "ocr-worker" for process in record["processes"]):
                raise RuntimeError("expected OCR worker process was not observed")
            if "observer" not in record:
                raise RuntimeError("process observation counter missing")
            records.append(record)
            record = None
        else:
            raise RuntimeError("invalid container metric frame")
    if record is not None or not records:
        raise RuntimeError("container sample missing or incomplete")
    return records


def private_file(path):
    # Exclusive creation refuses existing files and symlinks. Parent is private.
    return os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "w")


def emit(handle, record):
    handle.write(json.dumps(record, ensure_ascii=True, separators=(",", ":")) + "\n")
    handle.flush()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--container", required=True, help="Exact running OCR worker container name or ID")
    parser.add_argument("--output-dir", required=True, type=pathlib.Path, help="Private output directory; files must not exist")
    parser.add_argument("--duration-seconds", type=float, default=30)
    parser.add_argument("--interval-ms", type=int, default=200)
    parser.add_argument("--native-model-pid", type=positive_pid)
    parser.add_argument("--docker-vm-pid", type=positive_pid)
    args = parser.parse_args()
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,127}", args.container):
        parser.error("container must be an exact Docker name or ID, not options or a command")
    if not math.isfinite(args.duration_seconds) or not 0.2 <= args.duration_seconds <= 600:
        parser.error("duration must be 0.2..600 seconds")
    if not 200 <= args.interval_ms <= 600000:
        parser.error("interval must be at least 200 ms and at most 600000 ms")
    if args.interval_ms > args.duration_seconds * 1000:
        parser.error("interval cannot exceed duration")
    os.umask(0o077)
    args.output_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
    if args.output_dir.is_symlink() or args.output_dir.stat().st_mode & 0o077:
        parser.error("output directory must be a private directory (mode 700), not a symlink")
    count = min(3001, math.floor(args.duration_seconds * 1000 / args.interval_ms) + 1)
    started = time.monotonic()
    with private_file(args.output_dir / "worker-memory.jsonl") as worker, \
         private_file(args.output_dir / "host-memory.jsonl") as host, \
         private_file(args.output_dir / "summary.json") as summary:
        emit(worker, {"schema_version": SCHEMA, "kind": "configuration", "observed_at": utc_now(),
                      "container": args.container, "duration_seconds": args.duration_seconds,
                      "interval_ms": args.interval_ms, "sample_cap": count,
                      "peak_scope": "Current container lifetime; no peak reset was attempted.",
                      "process_identity": "Names only; multiple helpers remain separate rows; no PID or job labels."})
        emit(host, host_snapshot(args.native_model_pid, args.docker_vm_pid))
        finished = threading.Event()
        host_errors = []

        def observe_host():
            while not finished.wait(1):
                try:
                    emit(host, host_snapshot(args.native_model_pid, args.docker_vm_pid))
                except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
                    host_errors.append(type(error).__name__)
                    return

        observer = threading.Thread(target=observe_host, daemon=True)
        observer.start()
        success = False
        reason = "collector_failed"
        records = []
        try:
            process = subprocess.Popen(["docker", "exec", "-i", args.container, "sh", "-s", "--",
                                        str(round(args.duration_seconds * 1000)),
                                        f"{args.interval_ms / 1000:.3f}", str(count)],
                                       stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                       stderr=subprocess.PIPE, text=True)
            try:
                output, diagnostics = process.communicate(PROBE, timeout=args.duration_seconds + 30)
            except BaseException:
                process.kill()
                process.communicate()
                raise
            if process.returncode or diagnostics or len(output) > 32 * 1024 * 1024:
                raise RuntimeError("container collector failed; diagnostics withheld to avoid accidental disclosure")
            records = parse_probe(output)
            if len(records) > count:
                raise RuntimeError("sample cap exceeded")
            for record in records:
                emit(worker, record)
            finished.set()
            observer.join(timeout=25)
            if observer.is_alive() or host_errors:
                raise RuntimeError("host collector failed")
            emit(host, host_snapshot(args.native_model_pid, args.docker_vm_pid))
            success = True
            reason = None
        finally:
            finished.set()
            observer.join(timeout=25)
            emit(summary, {"schema_version": SCHEMA, "status": "passed" if success else "failed",
                           "reason": reason, "worker_samples": len(records),
                           "elapsed_seconds": round(time.monotonic() - started, 3),
                           "peak_current_bytes": max((r["cgroup"]["current"] for r in records), default=None),
                           "lifetime_peak_bytes": max((r["cgroup"]["peak"] for r in records), default=None),
                           "initial_current_bytes": records[0]["cgroup"]["current"] if records else None,
                           "final_current_bytes": records[-1]["cgroup"]["current"] if records else None,
                           "event_deltas": ({key: records[-1]["collections"]["memory.events"][key] - value
                                             for key, value in records[0]["collections"]["memory.events"].items()}
                                            if records else None),
                           "optional_process_unavailable_observations": sum(
                               r["observer"]["optional_processes_unavailable"] for r in records),
                           "limitations": ["Read-only observations; no workload or capacity certification.",
                                           "Container lifetime peak is not a per-job peak.",
                                           "Process RSS/PSS and unified GPU memory must not be added to cgroup totals.",
                                           "Host swap is not attributable to a particular job."]})
    print(f"Memory observations saved privately in {args.output_dir}")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"Memory collection failed ({type(error).__name__}); no successful measurement is claimed.", file=sys.stderr)
        sys.exit(1)
