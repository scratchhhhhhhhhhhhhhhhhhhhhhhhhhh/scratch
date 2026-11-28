"""Real Linux virtio-gpu DRM device discovery and DRM_IOCTL_VERSION round trip.
Boots the test kernel with --gpu-probe, opens /dev/dri/card0, runs
DRM_IOCTL_VERSION, and verifies the virtio-gpu DRM driver answered.
No accelerated GPU, no Mesa userspace, no Android, no timing certification.
"""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
sys.path.insert(0,str(Path(__file__).resolve().parent))
from native_initramfs import initramfs


def main():
    config=Path('kernel-artifact/kernel.config').read_text()
    required=['DRM','DRM_VIRTIO_GPU']
    missing=[name for name in required if f'CONFIG_{name}=y' not in config]
    if missing:
        raise RuntimeError(f'GPU acceptance requires the virtio-gpu DRM kernel: missing {missing}')
    exe=str(Path('rust/nexus-engine/target/release/nexus-engine.exe').resolve())
    kernel=Path('kernel-artifact/bzImage').resolve()
    expected=Path('kernel-artifact/SHA256SUMS.txt').read_text().split()[0]
    assert hashlib.sha256(kernel.read_bytes()).hexdigest()==expected
    rd=Path('gpu-initramfs.cpio')
    rd.write_bytes(initramfs(gpu=True))
    rd_sha=hashlib.sha256(rd.read_bytes()).hexdigest()
    # virtio_mmio.device=4K@0xd0000000:7 discovers all 7 MMIO slots
    # (block, net, audio, gpio, i2c, spi, gpu at 0xd0006000).
    cmd='earlyprintk=serial,ttyS0,115200 console=ttyS0,115200 nokaslr noapic nolapic acpi=off pci=off maxcpus=1 panic=-1 virtio_mmio.device=4K@0xd0000000:7'
    samples=[]
    for index in range(3):
        log=Path(f'gpu-serial-{index+1}.log')
        p=subprocess.run([exe,'boot-kernel',str(kernel),str(log),cmd,str(rd),
                          '--gpu-probe','--ram-mib=256'],
                         capture_output=True,text=True,timeout=45)
        serial=log.read_text(errors='replace') if log.exists() else ''
        timing=Path(str(log)+'.timing.json')
        metrics=json.loads(timing.read_text()) if timing.exists() else {}
        sample={'sample':index+1,'engine_exit':p.returncode,'stderr':p.stderr,
                'serial_tail':serial[-3500:],'metrics':metrics,'stdout':p.stdout}
        samples.append(sample)
        Path('gpu-checkpoint.json').write_text(json.dumps({'verified':False,'samples':samples},indent=2))
        print(json.dumps(sample,indent=2),flush=True)
        assert p.returncode==0,(p.stderr,serial[-3500:])
        assert 'NEXUS_DRM_VIRTIO_GPU_VERSION_OK' in serial,serial[-3500:]
        assert metrics.get('linux_userspace_ready_ms') is not None
        assert metrics['total_boot_verified'] is False and metrics['android_home_ready_ms'] is None

    result={'verified':True,
            'scope':'virtio-gpu DRM device discovery and DRM_IOCTL_VERSION round trip; no Mesa userspace, no accelerated GPU, no Android',
            'kernel_sha256':expected,'initramfs_sha256':rd_sha,'samples':samples}
    Path('gpu-checkpoint.json').write_text(json.dumps(result,indent=2))
    print('::notice::GPU_ACCEPTANCE='+json.dumps({'virtio_gpu_drm_discovery':True,'drm_ioctl_version_ok':True,
            'repetitions':3,'mesa_userspace':False,'accelerated_gpu':False,'android_ready':False}),flush=True)


if __name__=='__main__':
    try:
        main()
    except Exception as error:
        print('::error::'+str(error)[-3000:].replace('%','%25').replace('\r','%0D').replace('\n','%0A'),flush=True)
        raise
