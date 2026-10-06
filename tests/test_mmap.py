"""Regression coverage for FEX's low-hint / 4 GiB unmap failure."""
import ctypes
import os
from pathlib import Path
import unittest


class MmapCompatibilityTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        path = os.environ.get("MMAP_COMPAT_LIBRARY") or (
            Path(__file__).resolve().parents[1] / "build/runtime/native/dnf-binfmt-mmap.so")
        cls.library = ctypes.CDLL(str(path), use_errno=True)
        cls.mmap = cls.library.mmap
        cls.mmap.restype = ctypes.c_void_p
        cls.mmap.argtypes = [ctypes.c_void_p, ctypes.c_size_t, ctypes.c_int,
                            ctypes.c_int, ctypes.c_int, ctypes.c_long]
        cls.libc = ctypes.CDLL(None, use_errno=True)
        cls.libc.munmap.argtypes = [ctypes.c_void_p, ctypes.c_size_t]

    def test_cross_boundary_hint_is_relocated_and_can_be_unmapped(self):
        # Reserve virtual address space only; no RAM is committed.
        size = (1 << 32) + 0xff000
        address = self.mmap(0x1000, size, 0, 0x4022, -1, 0)
        self.assertNotEqual(address, ctypes.c_void_p(-1).value)
        self.assertGreaterEqual(address, 1 << 32)
        self.assertEqual(self.libc.munmap(address, size), 0)

    def test_small_exact_mapping_keeps_its_address(self):
        size = 4096
        # Probe a free high address, then request it with MAP_FIXED_NOREPLACE.
        address = self.mmap(None, size, 0, 0x22, -1, 0)
        self.assertNotEqual(address, ctypes.c_void_p(-1).value)
        self.assertEqual(self.libc.munmap(address, size), 0)
        fixed = self.mmap(address, size, 0, 0x100022, -1, 0)
        self.assertEqual(fixed, address)
        self.assertEqual(self.libc.munmap(fixed, size), 0)

    def test_syscall_errors_preserve_errno(self):
        ctypes.set_errno(0)
        self.assertEqual(self.mmap(None, 4096, 0, 2, -1, 0), ctypes.c_void_p(-1).value)
        self.assertEqual(ctypes.get_errno(), 9)  # EBADF

    def test_mmap64_uses_the_same_compatibility_entry_point(self):
        self.assertEqual(ctypes.cast(self.library.mmap64, ctypes.c_void_p).value,
                         ctypes.cast(self.library.mmap, ctypes.c_void_p).value)


if __name__ == "__main__":
    unittest.main()
