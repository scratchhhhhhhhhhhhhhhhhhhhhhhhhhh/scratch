"""Structural/oracle tests for diagnostic ELF/newc generation; not guest execution."""
import struct
import unittest
from native_initramfs import init_elf, initramfs


def unpack_archive(blob):
    files = {}
    at = 0
    while at < len(blob):
        header = blob[at:at+110]
        if len(header) != 110 or header[:6] != b'070701':
            raise ValueError('Invalid newc header')
        fields = [int(header[6+n*8:14+n*8], 16) for n in range(13)]
        name_size = fields[11]
        name_at = at+110
        name = blob[name_at:name_at+name_size]
        if not name or name[-1] != 0:
            raise ValueError('Missing name terminator')
        body_at = (name_at+name_size+3) & ~3
        body_end = body_at+fields[6]
        if body_end > len(blob):
            raise ValueError('Truncated newc body')
        key = name[:-1].decode()
        if key in files:
            raise ValueError('Duplicate archive name')
        files[key] = (fields, blob[body_at:body_end])
        at = (body_end+3) & ~3
        if key == 'TRAILER!!!':
            if at != len(blob):
                raise ValueError('Unexpected archive tail')
            return files
    raise ValueError('Missing trailer')


class FixtureTests(unittest.TestCase):
    def test_concurrent_direct_io_fixture_alignment_and_readonly_recovery(self):
        for count in (4,8):
            for readonly in (False,True):
                options=dict(disk_write=False,expected_cpus=count,stress_cpus=count,stress_readonly=readonly)
                elf=init_elf(**options)
                self.assertEqual(elf,init_elf(**options))
                self.assertEqual(unpack_archive(initramfs(**options))['init'][1],elf)
                self.assertIn(f'NEXUS_SMP_DIRECT_IO_STRESS_OK cpus={count} mode={"read" if readonly else "write"}'.encode(),elf)
                self.assertEqual(elf.count(b'\xb8\x39\x00\x00\x00\x0f\x05'),count)
                self.assertEqual(elf.count(b'\xb8\x12\x00\x00\x00'),0 if readonly else count)
                for cpu in range(count):
                    pattern=bytes((n*37+cpu*17+11)%256 for n in range(512))
                    offset=elf.find(pattern)
                    self.assertGreater(offset,120)
                    self.assertEqual((0x400000+offset)%4096,0)
        for options in (dict(stress_cpus=4),dict(disk_write=False,expected_cpus=1,stress_cpus=1),dict(disk_write=False,expected_cpus=4,stress_cpus=8)):
            with self.assertRaises(ValueError):init_elf(**options)

    def test_selected_cpu_affinity_fixture_is_deterministic_and_writable(self):
        import struct
        for count in range(1,9):
            elf=init_elf(expected_cpus=count)
            self.assertEqual(elf,init_elf(expected_cpus=count))
            self.assertIn(f'NEXUS_SMP_AFFINITY_OK cpus={count}'.encode(),elf)
            self.assertEqual(struct.unpack_from('<I',elf,68)[0],7)
            self.assertEqual(unpack_archive(initramfs(expected_cpus=count))['init'][1],elf)
        for count in (0,9,-1):
            with self.assertRaises(ValueError):init_elf(expected_cpus=count)

    def test_private_bridge_fixture_is_separate_from_default_guest(self):
        default=unpack_archive(initramfs())
        bridge=unpack_archive(initramfs(bridge=True))
        self.assertNotIn('dev/ttyS1',default)
        self.assertEqual(bridge['dev/ttyS1'][0][9:11],[4,65])
        elf=bridge['init'][1]
        self.assertEqual(struct.unpack_from('<I',elf,68)[0],7)
        for token in [b'/dev/ttyS1\0',b'NEXUS 1 QUERY_APP\n',b'NEXUS_PRIVATE_COM2_EXACT_QUERY_MATCH\n']:
            self.assertIn(token,elf)
        self.assertNotIn(b'NEXUS_PRIVATE_COM2_EXACT_QUERY_MATCH',default['init'][1])


    def test_elf_layout_and_archive_modes(self):
        for write, fs, clean in [(None, False, True), (True, False, True),
                                  (False, False, True), (True, True, True),
                                  (False, True, True), (True, True, False)]:
            with self.subTest(write=write, filesystem=fs, clean=clean):
                blob = initramfs(write, fs, clean)
                self.assertEqual(blob, initramfs(write, fs, clean))
                files = unpack_archive(blob)
                fields, elf = files['init']
                self.assertEqual(fields[1], 0o100755)
                self.assertEqual(elf, init_elf(write, fs, clean))
                self.assertEqual(elf[:7], b'\x7fELF\x02\x01\x01')
                self.assertEqual(struct.unpack_from('<Q', elf, 24)[0], 0x400078)
                segment = struct.unpack_from('<IIQQQQQQ', elf, 64)
                self.assertEqual(segment, (1, 5 if write is None else 7, 0,
                                          0x400000, 0x400000, len(elf), len(elf), 4096))
                self.assertEqual('mnt' in files, fs)
                self.assertEqual(files['dev/kmsg'][0][9:11], [1, 11])
                self.assertIn(b'NEXUS_USERSPACE_READY\n', elf)
                self.assertIn(b'NEXUS_MONOTONIC_4096_NO_BACKWARD_SUBMS_DELTA\n',elf)
                self.assertIn(b'\xb8\xe4\x00\x00\x00',elf) # real clock_gettime syscall
                self.assertIn(b'\x0f\x0b', elf)  # failure branch cannot emit success

    def test_read_boots_never_rewrite_expected_data(self):
        for fs in (False, True):
            with self.subTest(filesystem=fs):
                writer = init_elf(True, fs)
                reader = init_elf(False, fs)
                self.assertIn(b'\xb8\x12\x00\x00\x00\x44\x89\xe7', writer)
                self.assertNotIn(b'\xb8\x12\x00\x00\x00\x44\x89\xe7', reader)
                self.assertIn(b'\xb8\x11\x00\x00\x00\x44\x89\xe7', reader)
                self.assertIn(bytes((n*37+9) % 256 for n in range(512)), reader)
                self.assertIn(b'\xfc\xf3\xa6', reader)  # actual 512-byte comparison
                if fs:
                    self.assertIn(b'/mnt/probe.bin\0', reader)
                    self.assertIn(b'\x41\xba\x01\x00\x00\x00', reader)  # MS_RDONLY

    def test_display_fixture_has_real_fb_ioctl_and_checked_rows(self):
        blob=init_elf(display=True)
        self.assertIn(b'/dev/fb0\0',blob)
        self.assertIn(b'\xbe\x00\x46\x00\x00',blob)  # FBIOGET_VSCREENINFO
        self.assertIn(b'\x41\xbd\x58\x02\x00\x00',blob)  # 600 rows
        self.assertIn(b'\x3d\x80\x0c\x00\x00',blob)  # exact write length
        self.assertIn(b'NEXUS_FB0_PATTERN_WRITTEN\n',blob)
        self.assertIn(b'\x49\x89\xc6\x41\xc7\x86'+struct.pack('<I',800*600*4-4)+b'\xde\xad\xbe\xef',blob)
        self.assertIn(bytes(component for x in range(800) for component in ((x*3)%256,(x*5)%256,(x*7)%256,255)),blob)
        self.assertEqual(struct.unpack_from('<I',blob,68)[0],7)
        self.assertEqual(unpack_archive(initramfs(display=True))['init'][1],blob)

    def test_gpu_fixture_opens_drm_device_and_runs_version_ioctl(self):
        blob=init_elf(gpu=True)
        self.assertIn(b'/dev/dri/card0\x00',blob)
        self.assertIn(b'\xbe\x00\x44\x0c\xc0',blob)  # DRM_IOCTL_VERSION=0xC00C4400
        self.assertIn(b'\x8b\x3b',blob)  # mov edi,[r11] drm_major readback
        self.assertIn(b'\x85\xff',blob)  # test edi,edi
        self.assertIn(b'NEXUS_DRM_VIRTIO_GPU_VERSION_OK\n',blob)
        self.assertEqual(struct.unpack_from('<I',blob,68)[0],7)
        self.assertEqual(unpack_archive(initramfs(gpu=True))['init'][1],blob)
        # GPU + display combined must not duplicate devdir/devfs labels.
        combined=init_elf(display=True,gpu=True)
        self.assertIn(b'NEXUS_FB0_PATTERN_WRITTEN\n',combined)
        self.assertIn(b'NEXUS_DRM_VIRTIO_GPU_VERSION_OK\n',combined)
        self.assertEqual(unpack_archive(initramfs(display=True,gpu=True))['init'][1],combined)

    def test_unclean_stop_does_not_unmount_but_still_fsyncs(self):
        clean = init_elf(True, True, True)
        unclean = init_elf(True, True, False)
        self.assertIn(b'\xb8\xa6\x00\x00\x00', clean)
        self.assertNotIn(b'\xb8\xa6\x00\x00\x00', unclean)
        self.assertGreaterEqual(unclean.count(b'\xb8\x4a\x00\x00\x00'), 2)
        self.assertIn(b'NEXUS_EXT4_FSYNC_UNCLEAN_STOP_READY\n', unclean)
        self.assertNotIn(b'NEXUS_EXT4_FILE_FSYNC_UNMOUNT_MATCH\n', unclean)
        with self.assertRaises(ValueError):
            init_elf(filesystem=True)


if __name__ == '__main__':
    unittest.main()
