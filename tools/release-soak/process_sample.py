"""Platform sampler reused from accepted F5 memory controller; sampled values, not heap accounting."""
import ctypes
import os
from pathlib import Path
import subprocess
import sys

class Sampler:
    def __init__(self, pid):
        self.pid = pid
        self.handle = None
        if os.name == "nt":
            from ctypes import wintypes as wt

            class Memory(ctypes.Structure):
                _fields_ = [("cb", wt.DWORD), ("PageFaultCount", wt.DWORD)] + [
                    (name, ctypes.c_size_t) for name in (
                        "PeakWorkingSetSize", "WorkingSetSize", "QuotaPeakPagedPoolUsage",
                        "QuotaPagedPoolUsage", "QuotaPeakNonPagedPoolUsage", "QuotaNonPagedPoolUsage",
                        "PagefileUsage", "PeakPagefileUsage", "PrivateUsage")]

            self.memory_type = Memory
            self.kernel = ctypes.WinDLL("kernel32", use_last_error=True)
            self.psapi = ctypes.WinDLL("psapi", use_last_error=True)
            self.kernel.OpenProcess.argtypes = [wt.DWORD, wt.BOOL, wt.DWORD]
            self.kernel.OpenProcess.restype = wt.HANDLE
            self.kernel.CloseHandle.argtypes = [wt.HANDLE]
            self.kernel.GetProcessTimes.argtypes = [wt.HANDLE] + [ctypes.POINTER(wt.FILETIME)] * 4
            self.psapi.GetProcessMemoryInfo.argtypes = [wt.HANDLE, ctypes.POINTER(Memory), wt.DWORD]
            self.filetime = wt.FILETIME
            self.handle = self.kernel.OpenProcess(0x0400 | 0x0010, False, pid)
            if not self.handle:
                raise ctypes.WinError(ctypes.get_last_error())
        elif sys.platform.startswith("linux"):
            self.ticks = os.sysconf("SC_CLK_TCK")
        elif sys.platform != "darwin":
            raise RuntimeError("unsupported process sampler: " + sys.platform)

    def sample(self):
        if self.handle:
            memory = self.memory_type()
            memory.cb = ctypes.sizeof(memory)
            if not self.psapi.GetProcessMemoryInfo(self.handle, ctypes.byref(memory), memory.cb):
                raise ctypes.WinError(ctypes.get_last_error())
            times = [self.filetime() for _ in range(4)]
            if not self.kernel.GetProcessTimes(self.handle, *[ctypes.byref(item) for item in times]):
                raise ctypes.WinError(ctypes.get_last_error())
            cpu = sum((item.dwHighDateTime << 32) + item.dwLowDateTime for item in times[2:]) / 1e7
            return dict(rss=memory.WorkingSetSize, private=memory.PrivateUsage,
                        lifetime_peak_rss=memory.PeakWorkingSetSize, cpu_seconds=cpu)
        if sys.platform.startswith("linux"):
            folder = Path("/proc") / str(self.pid)
            status = {}
            for line in (folder / "status").read_text().splitlines():
                name, _, value = line.partition(":")
                if name in ("VmRSS", "VmHWM", "VmSize"):
                    status[name] = int(value.split()[0]) * 1024
            fields = (folder / "stat").read_text().rsplit(")", 1)[1].split()
            private = None
            try:
                private = sum(int(line.split()[1]) * 1024 for line in
                              (folder / "smaps_rollup").read_text().splitlines()
                              if line.startswith(("Private_Clean:", "Private_Dirty:")))
            except PermissionError:
                pass
            return dict(rss=status["VmRSS"], private=private, virtual=status["VmSize"],
                        lifetime_peak_rss=status["VmHWM"],
                        cpu_seconds=(int(fields[11]) + int(fields[12])) / self.ticks)
        # ps is intentionally sampled less often on Macs; no private/footprint claim.
        line = subprocess.check_output(
            ["ps", "-o", "rss=", "-o", "vsz=", "-o", "time=", "-p", str(self.pid)],
            text=True, timeout=2).split()
        parts = line[2].split(":")
        cpu = sum(float(value) * 60 ** index for index, value in enumerate(reversed(parts)))
        return dict(rss=int(line[0]) * 1024, private=None, virtual=int(line[1]) * 1024,
                    lifetime_peak_rss=None, cpu_seconds=cpu)

    def close(self):
        if self.handle:
            self.kernel.CloseHandle(self.handle)
            self.handle = None
