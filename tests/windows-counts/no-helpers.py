"""Run a usable Reader with no process slots left for curl, without touching system files."""

import ctypes
from ctypes import wintypes
import os
from pathlib import Path
import subprocess
import sys


class BasicLimits(ctypes.Structure):
    _fields_ = [
        ('per_process_time', ctypes.c_int64), ('per_job_time', ctypes.c_int64),
        ('flags', wintypes.DWORD), ('min_working_set', ctypes.c_size_t),
        ('max_working_set', ctypes.c_size_t), ('active_processes', wintypes.DWORD),
        ('affinity', ctypes.c_size_t), ('priority', wintypes.DWORD), ('scheduling', wintypes.DWORD),
    ]


kernel = ctypes.WinDLL('kernel32', use_last_error=True)
kernel.CreateJobObjectW.argtypes = [wintypes.LPVOID, wintypes.LPCWSTR]
kernel.CreateJobObjectW.restype = wintypes.HANDLE
kernel.SetInformationJobObject.argtypes = [wintypes.HANDLE, ctypes.c_int, wintypes.LPVOID, wintypes.DWORD]
kernel.AssignProcessToJobObject.argtypes = [wintypes.HANDLE, wintypes.HANDLE]
kernel.CloseHandle.argtypes = [wintypes.HANDLE]


def check(result):
    if not result:
        raise ctypes.WinError(ctypes.get_last_error())
    return result


job = check(kernel.CreateJobObjectW(None, None))
try:
    # JobObjectBasicLimitInformation: two slots, one for this launcher and one for the installed reader.
    limits = BasicLimits(flags=8, active_processes=2)
    check(kernel.SetInformationJobObject(job, 2, ctypes.byref(limits), ctypes.sizeof(limits)))
    check(kernel.AssignProcessToJobObject(job, wintypes.HANDLE(-1)))
    with open('CONIN$', 'rb', buffering=0) as stdin, open('CONOUT$', 'r+b', buffering=0) as stdout:
        with subprocess.Popen([sys.argv[1], 'view', 'sample.txt'], stdin=stdin, stdout=stdout, stderr=stdout) as reader:
            curl = Path(os.environ['SystemRoot']) / 'System32' / 'curl.exe'
            try:
                probe = subprocess.run([curl, '-q', '--version'], stdin=subprocess.DEVNULL,
                                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            except OSError as error:
                assert error.winerror in (5, 1816), f'Unexpected child creation failure: {error}'
            else:
                assert probe.returncode != 0, 'System curl could still start inside the job'
            assert reader.wait(timeout=45) == 0
finally:
    check(kernel.CloseHandle(job))
