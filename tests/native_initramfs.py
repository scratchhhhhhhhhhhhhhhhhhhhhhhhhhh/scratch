"""Deterministic dependency-free Linux diagnostic PID1 ELF/newc fixture.
Not Android, a shell, an installer or a production init system.
PID1 proves getpid and two real nanosleep returns via /dev/kmsg markers.
"""
import struct


def init_elf(disk_write=None, filesystem=False, clean_unmount=True, display=False, bridge=False, expected_cpus=None, stress_cpus=None, stress_readonly=False, gpu=False):
    if stress_cpus is not None and (not isinstance(stress_cpus,int) or not 2<=stress_cpus<=8 or disk_write is None or filesystem or expected_cpus!=stress_cpus):
        raise ValueError("Direct IO stress requires matching 2..8 CPUs and a raw block fixture")
    if filesystem and disk_write is None:
        raise ValueError("Filesystem fixture requires an explicit write/read mode")
    base = 0x400000
    entry = base + 120
    code = bytearray()
    labels = {}
    absolute = []
    relative = []

    def emit(data): code.extend(data)
    def label(name): labels[name] = len(code)
    def address(prefix, name):
        emit(prefix)
        absolute.append((len(code), name))
        emit(bytes(8))
    def branch(op, name):
        emit(op)
        relative.append((len(code), name))
        emit(bytes(4))
    def log(name, data):
        emit(b'\xb8\x01\x00\x00\x00\x89\xef')  # write(fd saved in EBP)
        address(b'\x48\xbe', name)
        emit(b'\xba' + struct.pack('<I', len(data)) + b'\x0f\x05')
        emit(b'\x3d' + struct.pack('<I', len(data)))
        branch(b'\x0f\x85', 'fail')

    emit(b'\xb8\x27\x00\x00\x00\x0f\x05\x83\xf8\x01')  # getpid() == 1
    branch(b'\x0f\x85', 'fail')
    emit(b'\xb8\x02\x00\x00\x00')  # open /dev/kmsg O_WRONLY
    address(b'\x48\xbf', 'device')
    emit(b'\xbe\x01\x00\x00\x00\x31\xd2\x0f\x05\x85\xc0')
    branch(b'\x0f\x88', 'fail')
    emit(b'\x89\xc5')  # EBP=fd
    started = b'NEXUS_USERSPACE_PID1\n'
    tick = b'NEXUS_USERSPACE_TICK\n'
    ready = b'NEXUS_USERSPACE_READY\n'
    log('started', started)
    if expected_cpus is not None:
        if not isinstance(expected_cpus,int) or not 1<=expected_cpus<=8:
            raise ValueError("Expected CPU count must be 1..8")
        for cpu in range(expected_cpus):
            # Linux sched_setaffinity(0,8,&mask), then getcpu(&cpu,NULL,NULL).
            emit(b'\xb8\xcb\x00\x00\x00\x31\xff\xbe\x08\x00\x00\x00')
            address(b'\x48\xba',f'cpumask{cpu}')
            emit(b'\x0f\x05\x85\xc0');branch(b'\x0f\x85','fail')
            emit(b'\xb8\x35\x01\x00\x00');address(b'\x48\xbf','currentcpu')
            emit(b'\x31\xf6\x31\xd2\x0f\x05\x85\xc0');branch(b'\x0f\x85','fail')
            address(b'\x48\xb8','currentcpu');emit(b'\x83\x38'+bytes([cpu]));branch(b'\x0f\x85','fail')
        smpmarker=f'NEXUS_SMP_AFFINITY_OK cpus={expected_cpus}\n'.encode()
        log('smpmarker',smpmarker)

    # Exercise the real kernel clock syscall, not RDTSC or a host timestamp.
    # Stack output remains writable even for the default read/execute ELF.
    emit(b'\x48\x83\xec\x10\x45\x31\xf6')  # reserve timespec; r14d=fine delta seen
    def clock_sample():
        emit(b'\xb8\xe4\x00\x00\x00\xbf\x01\x00\x00\x00\x48\x89\xe6\x0f\x05\x85\xc0')
        branch(b'\x0f\x85','fail')
        emit(b'\x48\x8b\x04\x24\x48\x69\xc0'+struct.pack('<I',1_000_000_000)+b'\x48\x03\x44\x24\x08')
    clock_sample()
    emit(b'\x49\x89\xc4\xbb\x00\x10\x00\x00')  # previous timestamp, 4096 samples
    label('clocksample')
    clock_sample()
    emit(b'\x4c\x39\xe0');branch(b'\x0f\x82','fail')  # no backward clock
    emit(b'\x49\x89\xc5\x4c\x29\xe0\x48\x85\xc0')
    branch(b'\x0f\x84','clocknext')  # equality allowed, but not for every sample
    emit(b'\x48\x3d'+struct.pack('<I',1_000_000));branch(b'\x0f\x83','clocknext')
    emit(b'\x41\xbe\x01\x00\x00\x00')  # observed positive delta below 1 ms
    label('clocknext')
    emit(b'\x4d\x89\xec\xff\xcb');branch(b'\x0f\x85','clocksample')
    emit(b'\x45\x85\xf6');branch(b'\x0f\x84','fail')
    emit(b'\x48\x83\xc4\x10')
    clockmarker=b'NEXUS_MONOTONIC_4096_NO_BACKWARD_SUBMS_DELTA\n'
    log('clockmarker',clockmarker)
    emit(b'\xbb\x02\x00\x00\x00')  # two successful sleeps
    label('sleep')
    emit(b'\xb8\x23\x00\x00\x00')
    address(b'\x48\xbf', 'timespec')
    emit(b'\x31\xf6\x0f\x05\x83\xf8\xfc')  # EINTR: retry, never count as success
    branch(b'\x0f\x84', 'sleep')
    emit(b'\x85\xc0')
    branch(b'\x0f\x85', 'fail')
    log('tick', tick)
    emit(b'\xff\xcb')
    branch(b'\x0f\x85', 'sleep')
    if disk_write is not None:
        # Mount devtmpfs instead of guessing virtio-blk's dynamically assigned major.
        emit(b'\xb8\xa5\x00\x00\x00\x31\xff')
        address(b'\x48\xbe', 'devdir')
        address(b'\x48\xba', 'devfs')
        emit(b'\x45\x31\xd2\x45\x31\xc0\x0f\x05\x85\xc0')
        branch(b'\x0f\x85', 'fail')
        if filesystem:
            emit(b'\xb8\xa5\x00\x00\x00')
            address(b'\x48\xbf', 'blockdev')
            address(b'\x48\xbe', 'mountpoint')
            address(b'\x48\xba', 'fstype')
            emit(b'\x41\xba'+struct.pack('<I', 0 if disk_write else 1))
            emit(b'\x45\x31\xc0\x0f\x05\x85\xc0')
            branch(b'\x0f\x85', 'fail')
        emit(b'\xb8\x02\x00\x00\x00')
        address(b'\x48\xbf', 'filename' if filesystem else 'blockdev')
        emit(b'\xbe'+struct.pack('<I', (0x242 if disk_write else 0) if filesystem else 2))
        emit(b'\xba\x80\x01\x00\x00\x0f\x05\x85\xc0')
        branch(b'\x0f\x88', 'fail')
        emit(b'\x41\x89\xc4')  # r12d = block fd, ebp still holds kmsg fd
        if disk_write:
            emit(b'\xb8\x12\x00\x00\x00\x44\x89\xe7')  # pwrite64
            address(b'\x48\xbe', 'pattern')
            emit(b'\xba\x00\x02\x00\x00\x41\xba'+struct.pack('<I', 0 if filesystem else 4096)+b'\x0f\x05\x3d\x00\x02\x00\x00')
            branch(b'\x0f\x85', 'fail')
            emit(b'\xb8\x4a\x00\x00\x00\x44\x89\xe7\x0f\x05\x85\xc0')  # fsync
            branch(b'\x0f\x85', 'fail')
        emit(b'\xb8\x11\x00\x00\x00\x44\x89\xe7')  # pread64
        address(b'\x48\xbe', 'readbuffer')
        emit(b'\xba\x00\x02\x00\x00\x41\xba'+struct.pack('<I', 0 if filesystem else 4096)+b'\x0f\x05\x3d\x00\x02\x00\x00')
        branch(b'\x0f\x85', 'fail')
        address(b'\x48\xbe', 'pattern')
        address(b'\x48\xbf', 'readbuffer')
        emit(b'\xb9\x00\x02\x00\x00\xfc\xf3\xa6')
        branch(b'\x0f\x85', 'fail')
        emit(b'\xb8\x03\x00\x00\x00\x44\x89\xe7\x0f\x05\x85\xc0')
        branch(b'\x0f\x85', 'fail')
        if filesystem:
            if disk_write:
                # fsync the parent directory as well as the file, then cleanly unmount.
                emit(b'\xb8\x02\x00\x00\x00')
                address(b'\x48\xbf', 'mountpoint')
                emit(b'\xbe\x00\x00\x01\x00\x31\xd2\x0f\x05\x85\xc0')
                branch(b'\x0f\x88', 'fail')
                emit(b'\x41\x89\xc4\x89\xc7\xb8\x4a\x00\x00\x00\x0f\x05\x85\xc0')
                branch(b'\x0f\x85', 'fail')
                emit(b'\x44\x89\xe7\xb8\x03\x00\x00\x00\x0f\x05\x85\xc0')
                branch(b'\x0f\x85', 'fail')
            if clean_unmount:
                emit(b'\xb8\xa6\x00\x00\x00')
                address(b'\x48\xbf', 'mountpoint')
                emit(b'\x31\xf6\x0f\x05\x85\xc0')
                branch(b'\x0f\x85', 'fail')
        disk_marker=(b'NEXUS_EXT4_FILE_FSYNC_UNMOUNT_MATCH\n' if disk_write else b'NEXUS_EXT4_REBOOT_FILE_MATCH\n') if filesystem else (b'NEXUS_VDA_WRITE_FLUSH_READ_MATCH\n' if disk_write else b'NEXUS_VDA_REBOOT_READ_MATCH\n')
        if filesystem and disk_write and not clean_unmount:
            disk_marker=b'NEXUS_EXT4_FSYNC_UNCLEAN_STOP_READY\n'
        log('diskmarker', disk_marker)
    if stress_cpus is not None:
        # Fork every child before waiting, so pinned processes issue I/O concurrently.
        for cpu in range(stress_cpus):
            emit(b'\xb8\x39\x00\x00\x00\x0f\x05\x48\x85\xc0')
            branch(b'\x0f\x88','fail');branch(b'\x0f\x84',f'stresschild{cpu}')
        emit(b'\xbb'+struct.pack('<I',stress_cpus))
        label('stresswait')
        emit(b'\xb8\x3d\x00\x00\x00\x48\xc7\xc7\xff\xff\xff\xff')
        address(b'\x48\xbe','stressstatus');emit(b'\x31\xd2\x45\x31\xd2\x0f\x05\x48\x85\xc0')
        branch(b'\x0f\x8e','fail')
        address(b'\x48\xb8','stressstatus');emit(b'\x83\x38\x00');branch(b'\x0f\x85','fail')
        emit(b'\xff\xcb');branch(b'\x0f\x85','stresswait')
        stressmarker=f'NEXUS_SMP_DIRECT_IO_STRESS_OK cpus={stress_cpus} mode={"read" if stress_readonly else "write"}\n'.encode()
        log('stressmarker',stressmarker);branch(b'\xe9','stressdone')
        for cpu in range(stress_cpus):
            label(f'stresschild{cpu}')
            emit(b'\xb8\xcb\x00\x00\x00\x31\xff\xbe\x08\x00\x00\x00')
            address(b'\x48\xba',f'cpumask{cpu}');emit(b'\x0f\x05\x85\xc0');branch(b'\x0f\x85','fail')
            emit(b'\xb8\x02\x00\x00\x00');address(b'\x48\xbf','blockdev')
            emit(b'\xbe\x02\x40\x00\x00\x31\xd2\x0f\x05\x85\xc0');branch(b'\x0f\x88','fail')
            emit(b'\x41\x89\xc4\xbb'+struct.pack('<I',1 if stress_readonly else 16))
            label(f'stressloop{cpu}')
            if not stress_readonly:
                emit(b'\xb8\x12\x00\x00\x00\x44\x89\xe7');address(b'\x48\xbe',f'stresspattern{cpu}')
                emit(b'\xba\x00\x02\x00\x00\x41\xba'+struct.pack('<I',8192+cpu*4096)+b'\x0f\x05\x3d\x00\x02\x00\x00')
                branch(b'\x0f\x85','fail')
                emit(b'\xb8\x4a\x00\x00\x00\x44\x89\xe7\x0f\x05\x85\xc0');branch(b'\x0f\x85','fail')
            emit(b'\xb8\x11\x00\x00\x00\x44\x89\xe7');address(b'\x48\xbe','stressbuffer')
            emit(b'\xba\x00\x02\x00\x00\x41\xba'+struct.pack('<I',8192+cpu*4096)+b'\x0f\x05\x3d\x00\x02\x00\x00')
            branch(b'\x0f\x85','fail')
            address(b'\x48\xbe',f'stresspattern{cpu}');address(b'\x48\xbf','stressbuffer')
            emit(b'\xb9\x00\x02\x00\x00\xfc\xf3\xa6');branch(b'\x0f\x85','fail')
            emit(b'\xff\xcb');branch(b'\x0f\x85',f'stressloop{cpu}')
            emit(b'\xb8\x03\x00\x00\x00\x44\x89\xe7\x0f\x05\x85\xc0');branch(b'\x0f\x85','fail')
            emit(b'\xb8\xe7\x00\x00\x00\x31\xff\x0f\x05\x0f\x0b')
        label('stressdone')
    if display:
        if disk_write is None:
            emit(b'\xb8\xa5\x00\x00\x00\x31\xff')
            address(b'\x48\xbe', 'devdir')
            address(b'\x48\xba', 'devfs')
            emit(b'\x45\x31\xd2\x45\x31\xc0\x0f\x05\x85\xc0')
            branch(b'\x0f\x85', 'fail')
        emit(b'\xb8\x02\x00\x00\x00')
        address(b'\x48\xbf', 'fbdev')
        emit(b'\xbe\x02\x00\x00\x00\x31\xd2\x0f\x05\x85\xc0')
        branch(b'\x0f\x88', 'fail')
        emit(b'\x41\x89\xc4\x89\xc7\xb8\x10\x00\x00\x00\xbe\x00\x46\x00\x00')
        address(b'\x48\xba', 'fbinfo')
        emit(b'\x0f\x05\x85\xc0')
        branch(b'\x0f\x85', 'fail')
        address(b'\x48\xbb', 'fbinfo')
        for instruction in (b'\x81\x3b\x20\x03\x00\x00',b'\x81\x7b\x04\x58\x02\x00\x00',b'\x81\x7b\x18\x20\x00\x00\x00'):
            emit(instruction)
            branch(b'\x0f\x85', 'fail')
        emit(b'\x41\xbd\x58\x02\x00\x00')
        label('drawrow')
        emit(b'\xb8\x01\x00\x00\x00\x44\x89\xe7')
        address(b'\x48\xbe', 'fbrow')
        emit(b'\xba\x80\x0c\x00\x00\x0f\x05\x3d\x80\x0c\x00\x00')
        branch(b'\x0f\x85', 'fail')
        # Tint each next row differently so host comparison catches stride/vertical flips.
        address(b'\x48\xbb', 'fbrow')
        emit(b'\x48\xff\xc3\xb9\x20\x03\x00\x00')
        label('tintrow')
        emit(b'\xfe\x03\x48\x83\xc3\x04\xff\xc9')
        branch(b'\x0f\x85', 'tintrow')
        emit(b'\x41\xff\xcd')
        branch(b'\x0f\x85', 'drawrow')
        # mmap the actual fbdev surface and overwrite the final pixel through guest paging.
        emit(b'\xb8\x09\x00\x00\x00\x31\xff\xbe\x00\x50\x1d\x00\xba\x03\x00\x00\x00')
        emit(b'\x41\xba\x01\x00\x00\x00\x45\x89\xe0\x45\x31\xc9\x0f\x05\x48\x85\xc0')
        branch(b'\x0f\x88', 'fail')
        emit(b'\x49\x89\xc6\x41\xc7\x86'+struct.pack('<I',800*600*4-4)+b'\xde\xad\xbe\xef')
        emit(b'\x41\x81\xbe'+struct.pack('<I',800*600*4-4)+b'\xde\xad\xbe\xef')
        branch(b'\x0f\x85', 'fail')
        emit(b'\xb8\x0b\x00\x00\x00\x4c\x89\xf7\xbe\x00\x50\x1d\x00\x0f\x05\x85\xc0')
        branch(b'\x0f\x85', 'fail')
        emit(b'\xb8\x03\x00\x00\x00\x44\x89\xe7\x0f\x05\x85\xc0')
        branch(b'\x0f\x85', 'fail')
        display_marker=b'NEXUS_FB0_PATTERN_WRITTEN\n'
        log('displaymarker',display_marker)
    if gpu:
        if disk_write is None and not display:
            emit(b'\xb8\xa5\x00\x00\x00\x31\xff')
            address(b'\x48\xbe', 'devdir')
            address(b'\x48\xba', 'devfs')
            emit(b'\x45\x31\xd2\x45\x31\xc0\x0f\x05\x85\xc0')
            branch(b'\x0f\x85', 'fail')
        # open("/dev/dri/card0", O_RDWR) — the DRM node created by the
        # virtio-gpu driver when it probes the VirtIO-MMIO GPU device.
        emit(b'\xb8\x02\x00\x00\x00')
        address(b'\x48\xbf', 'dridev')
        emit(b'\xbe\x02\x00\x00\x00\x31\xd2\x0f\x05\x85\xc0')
        branch(b'\x0f\x88', 'fail')
        emit(b'\x41\x89\xc4')  # r12d = dri fd
        # ioctl(fd, DRM_IOCTL_VERSION=0xC00C4400, &drm_version)
        # DRM_IOCTL_VERSION = _IOWR('D', 0, struct drm_version{int,int,int})
        emit(b'\xb8\x10\x00\x00\x00')  # mov eax, 16 (ioctl)
        emit(b'\x44\x89\xe7')  # mov rdi, r12d (fd)
        emit(b'\xbe\x00\x44\x0c\xc0')  # mov esi, 0xC00C4400 (DRM_IOCTL_VERSION)
        address(b'\x48\xba', 'drmversion')  # mov rdx, &drm_version
        emit(b'\x0f\x05\x85\xc0')  # syscall; test eax,eax
        branch(b'\x0f\x85', 'fail')
        # verify drm_version.drm_major > 0 (a real DRM driver answered)
        address(b'\x48\xbb', 'drmversion')  # r11 = &drm_version
        emit(b'\x8b\x3b')  # mov edi, [r11] (drm_major)
        emit(b'\x85\xff')  # test edi, edi
        branch(b'\x0f\x84', 'fail')  # jz fail (major == 0)
        # close(fd)
        emit(b'\xb8\x03\x00\x00\x00')  # mov eax, 3 (close)
        emit(b'\x44\x89\xe7')  # mov rdi, r12d (fd)
        emit(b'\x0f\x05\x85\xc0')  # syscall; test eax,eax
        branch(b'\x0f\x85', 'fail')
        gpu_marker=b'NEXUS_DRM_VIRTIO_GPU_VERSION_OK\n'
        log('gpumarker',gpu_marker)
    if bridge:
        emit(b'\xb8\x02\x00\x00\x00')
        address(b'\x48\xbf','bridgepath')
        emit(b'\xbe\x02\x00\x00\x00\x31\xd2\x0f\x05\x85\xc0')
        branch(b'\x0f\x88','fail')
        emit(b'\x41\x89\xc4')  # r12d=private tty fd
        emit(b'\xb8\x10\x00\x00\x00\x44\x89\xe7\xbe\x02\x54\x00\x00')
        address(b'\x48\xba','bridgetermios')
        emit(b'\x0f\x05\x85\xc0');branch(b'\x0f\x85','fail')
        def bridge_write(name,body):
            emit(b'\xb8\x01\x00\x00\x00\x44\x89\xe7')
            address(b'\x48\xbe',name)
            emit(b'\xba'+struct.pack('<I',len(body))+b'\x0f\x05\x3d'+struct.pack('<I',len(body)))
            branch(b'\x0f\x85','fail')
        greeting=b'NEXUS 1 HELLO READY NAV VOLUME SHUTDOWN HOME_CHECK APP APK\n'
        answer=b'NEXUS 1 APP org.nexus.fixture\nNEXUS 1 RESULT QUERY_APP OK\n'
        query=b'NEXUS 1 QUERY_APP\n'
        bridge_write('bridgehello',greeting)
        address(b'\x49\xbe','bridgequery')  # r14 expected pointer
        emit(b'\x41\xbf'+struct.pack('<I',len(query)))
        label('bridgeread')
        emit(b'\x31\xc0\x44\x89\xe7');address(b'\x48\xbe','bridgebyte')
        emit(b'\xba\x01\x00\x00\x00\x0f\x05\x48\x83\xf8\x01');branch(b'\x0f\x85','fail')
        address(b'\x48\xbb','bridgebyte');emit(b'\x8a\x03\x41\x3a\x06');branch(b'\x0f\x85','fail')
        emit(b'\x49\xff\xc6\x41\xff\xcf');branch(b'\x0f\x85','bridgeread')
        bridge_marker=b'NEXUS_PRIVATE_COM2_EXACT_QUERY_MATCH\n'
        log('bridgemarker',bridge_marker)
        bridge_write('bridgeanswer',answer)
    log('ready', ready)
    label('pause')
    emit(b'\xb8\x22\x00\x00\x00\x0f\x05')
    branch(b'\xe9', 'pause')
    label('fail')
    emit(b'\x0f\x0b')  # visible PID1 failure, never a success marker
    for name, data in [('device', b'/dev/kmsg\0'), ('timespec', struct.pack('<qq', 0, 100_000_000)),
                       ('started', started), ('tick', tick), ('ready', ready), ('clockmarker',clockmarker)]:
        label(name)
        emit(data)
    if disk_write is not None:
        for name,data in [('devdir',b'/dev\0'),('devfs',b'devtmpfs\0'),('blockdev',b'/dev/vda\0'),
                          ('pattern',bytes((n*37+9)%256 for n in range(512))),('readbuffer',bytes(512)),('diskmarker',disk_marker)]:
            label(name)
            emit(data)
    if filesystem:
        for name,data in [('mountpoint',b'/mnt\0'),('fstype',b'ext4\0'),('filename',b'/mnt/probe.bin\0')]:
            label(name)
            emit(data)
    if display:
        if disk_write is None:
            for name,data in [('devdir',b'/dev\0'),('devfs',b'devtmpfs\0')]:
                label(name)
                emit(data)
        for name,data in [('fbdev',b'/dev/fb0\0'),('fbinfo',bytes(160)),
                          ('fbrow',bytes(component for x in range(800) for component in ((x*3)%256,(x*5)%256,(x*7)%256,255))),
                          ('displaymarker',display_marker)]:
            label(name)
            emit(data)
    if gpu:
        if disk_write is None and not display:
            for name,data in [('devdir',b'/dev\0'),('devfs',b'devtmpfs\0')]:
                label(name);emit(data)
        for name,data in [('dridev',b'/dev/dri/card0\0'),('drmversion',bytes(12)),
                          ('gpumarker',gpu_marker)]:
            label(name);emit(data)
    if bridge:
        cc=bytearray(20);cc[7]=1  # c_line then NCCS: VMIN index 6.
        for name,body in [('bridgepath',b'/dev/ttyS1\0'),('bridgetermios',struct.pack('<IIII',0,0,0x8bd,0)+cc),
                          ('bridgehello',greeting),('bridgequery',query),('bridgeanswer',answer),
                          ('bridgebyte',bytes(1)),('bridgemarker',bridge_marker)]:
            label(name);emit(body)
    if expected_cpus is not None:
        label('currentcpu');emit(bytes(4))
        label('smpmarker');emit(smpmarker)
        for cpu in range(expected_cpus):
            label(f'cpumask{cpu}');emit(struct.pack('<Q',1<<cpu))
    if stress_cpus is not None:
        label('stressstatus');emit(bytes(4));label('stressmarker');emit(stressmarker)
        # O_DIRECT requires aligned user buffers, not merely aligned disk offsets.
        for cpu in range(stress_cpus):
            emit(bytes(-(entry+len(code))%4096));label(f'stresspattern{cpu}')
            emit(bytes((n*37+cpu*17+11)%256 for n in range(512)))
        emit(bytes(-(entry+len(code))%4096));label('stressbuffer');emit(bytes(512))
    for offset, name in absolute:
        struct.pack_into('<Q', code, offset, entry + labels[name])
    for offset, name in relative:
        struct.pack_into('<i', code, offset, labels[name] - offset - 4)
    size = 120 + len(code)
    header = struct.pack('<16sHHIQQQIHHHHHH', b'\x7fELF\x02\x01\x01'+bytes(9), 2, 62, 1,
                         entry, 64, 0, 0, 64, 56, 1, 0, 0, 0)
    segment = struct.pack('<IIQQQQQQ', 1, 7 if disk_write is not None or display or bridge or expected_cpus is not None or gpu else 5, 0, base, base, size, size, 4096)
    return header + segment + code


def newc(name, mode, body=b'', rmajor=0, rminor=0, inode=1):
    encoded = name.encode() + b'\0'
    fields = [inode, mode, 0, 0, 1, 0, len(body), 0, 0, rmajor, rminor, len(encoded), 0]
    data = b'070701' + b''.join(f'{v:08x}'.encode() for v in fields) + encoded
    data += bytes(-len(data) % 4)
    data += body
    return data + bytes(-len(data) % 4)


def initramfs(disk_write=None, filesystem=False, clean_unmount=True, display=False, bridge=False, expected_cpus=None, stress_cpus=None, stress_readonly=False, gpu=False):
    return (newc('dev', 0o40755, inode=1) + newc('dev/console', 0o20600, rmajor=5, rminor=1, inode=2)
            + newc('dev/kmsg', 0o20600, rmajor=1, rminor=11, inode=3)
            + (newc('dev/ttyS1',0o20600,rmajor=4,rminor=65,inode=7) if bridge else b'')
            + (newc('dev/dri', 0o40755, inode=8) if gpu else b'')
            + (newc('mnt', 0o40755, inode=6) if filesystem else b'')
            + newc('init', 0o100755, init_elf(disk_write, filesystem, clean_unmount, display, bridge, expected_cpus, stress_cpus, stress_readonly, gpu), inode=4) + newc('TRAILER!!!', 0, inode=5))


if __name__ == '__main__':
    import argparse
    from pathlib import Path
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('output')
    args = p.parse_args()
    with Path(args.output).open('xb') as stream:
        stream.write(initramfs())
