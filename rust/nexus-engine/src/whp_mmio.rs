// Included inside whp::windows. Callback lifetimes are limited to synchronous emulation.
#[repr(C)]
struct EmulatorMemory {gpa:u64,direction:u8,size:u8,data:[u8;8]}
#[repr(C)]
struct EmulatorCallbacks {
    size:u32,reserved:u32,
    io:extern "system" fn(*mut c_void,*mut c_void)->i32,
    memory:extern "system" fn(*mut c_void,*mut EmulatorMemory)->i32,
    get:extern "system" fn(*mut c_void,*const u32,u32,*mut Register)->i32,
    set:extern "system" fn(*mut c_void,*const u32,u32,*const Register)->i32,
    translate:extern "system" fn(*mut c_void,u64,u32,*mut u32,*mut u64)->i32,
}
#[link(name="WinHvEmulation")]
extern "system" {
    fn WHvEmulatorCreateEmulator(callbacks:*const EmulatorCallbacks,emulator:*mut *mut c_void)->i32;
    fn WHvEmulatorDestroyEmulator(emulator:*mut c_void)->i32;
    fn WHvEmulatorTryMmioEmulation(emulator:*mut c_void,context:*mut c_void,vp:*const u8,access:*const u8,status:*mut u32)->i32;
}
#[repr(C)]
struct Translation {code:u32,reserved:u32}
#[link(name="WinHvPlatform")]
extern "system" {
    fn WHvTranslateGva(partition:*mut c_void,index:u32,gva:u64,flags:u32,result:*mut Translation,gpa:*mut u64)->i32;
}
const MMIO_BASE:u64=0xd0000000;
/// Secondary VirtIO-MMIO base for the net device; distinct from the block base so
/// both devices can coexist under one emulator callback.
const MMIO_NET_BASE:u64=0xd0001000;
/// Third VirtIO-MMIO base for the audio device; distinct so all three devices
/// can coexist under one emulator callback.
const MMIO_AUDIO_BASE:u64=0xd0002000;
/// Fourth MMIO base for the GPIO controller.
const MMIO_GPIO_BASE:u64=0xd0003000;
/// Fifth MMIO base for the I2C controller.
const MMIO_I2C_BASE:u64=0xd0004000;
/// Sixth MMIO base for the SPI controller.
const MMIO_SPI_BASE:u64=0xd0005000;
/// Seventh MMIO base for the VirtIO-GPU device.
const MMIO_GPU_BASE:u64=0xd0006000;
/// Which VirtIO-MMIO device the context services. Block is the default so the
/// existing disk boot path is unchanged; Net and Audio are activated only when
/// requested (wiring in place; boot plans activate them via flags).
enum MmioDevice {
    Block(crate::virtio_mmio::BlockMmio),
    #[allow(dead_code)]
    Net(crate::virtio_net_mmio::NetMmio),
    #[allow(dead_code)]
    Audio(crate::virtio_audio_mmio::AudioMmio),
    #[allow(dead_code)]
    Gpio(crate::gpio::GpioController),
    #[allow(dead_code)]
    I2c(crate::i2c::I2cController),
    #[allow(dead_code)]
    Spi(crate::spi::SpiController),
    #[allow(dead_code)]
    Gpu(crate::virtio_gpu_mmio::GpuMmio),
    /// Absent device: returns the VirtIO "no device" magic (0xFFFFFFFF) for
    /// device-ID reads so the kernel's virtio-mmio probe fails gracefully and
    /// skips this slot instead of crashing the VM.
    Null,
}
impl MmioDevice {
    fn read(&self,offset:u64,size:u8)->Result<u32,String> {match self {MmioDevice::Block(d)=>d.read(offset,size),MmioDevice::Net(d)=>d.read(offset,size),MmioDevice::Audio(d)=>d.read(offset,size),MmioDevice::Gpio(d)=>d.read(offset,size),MmioDevice::I2c(d)=>d.read(offset,size),MmioDevice::Spi(d)=>d.read(offset,size),MmioDevice::Gpu(d)=>d.read(offset,size),MmioDevice::Null=>Ok(0xFFFF_FFFF)}}
    fn write(&mut self,offset:u64,size:u8,value:u32)->Result<(),String> {match self {MmioDevice::Block(d)=>d.write(offset,size,value),MmioDevice::Net(d)=>d.write(offset,size,value),MmioDevice::Audio(d)=>d.write(offset,size,value),MmioDevice::Gpio(d)=>d.write(offset,size,value),MmioDevice::I2c(d)=>d.write(offset,size,value),MmioDevice::Spi(d)=>d.write(offset,size,value),MmioDevice::Gpu(d)=>d.write(offset,size,value),MmioDevice::Null=>Ok(())}}
    fn work_pending(&self)->bool {match self {MmioDevice::Block(d)=>d.work_pending(),MmioDevice::Net(d)=>d.work_pending(),MmioDevice::Audio(d)=>d.work_pending(),MmioDevice::Gpio(d)=>d.work_pending(),MmioDevice::I2c(d)=>d.work_pending(),MmioDevice::Spi(d)=>d.work_pending(),MmioDevice::Gpu(d)=>d.work_pending(),MmioDevice::Null=>false}}
    fn irq_asserted(&self)->bool {match self {MmioDevice::Block(d)=>d.irq_asserted(),MmioDevice::Net(d)=>d.irq_asserted(),MmioDevice::Audio(d)=>d.irq_asserted(),MmioDevice::Gpio(d)=>d.irq_asserted(),MmioDevice::I2c(d)=>d.irq_asserted(),MmioDevice::Spi(d)=>d.irq_asserted(),MmioDevice::Gpu(d)=>d.irq_asserted(),MmioDevice::Null=>false}}
}
struct MmioContext {
    partition:*mut c_void,ram:*mut u8,bytes:usize,device:MmioDevice,
    error:Option<String>,translations:u64,accesses:u64,
}
fn callback(context:*mut c_void,action:impl FnOnce(&mut MmioContext)->Result<(),String>)->i32 {
    if context.is_null() {return 0x80004003u32 as i32;}
    // Context is our stack-owned object; SDK callbacks are synchronous and never retained.
    let ctx=unsafe {&mut *context.cast::<MmioContext>()};
    if ctx.error.is_some() {return 0x80004005u32 as i32;}
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(||action(ctx))) {
        Ok(Ok(()))=>0,
        Ok(Err(error))=>{ctx.error=Some(error);0x80004005u32 as i32},
        Err(_)=>{ctx.error=Some("Panic contained inside MMIO emulator callback".into());0x80004005u32 as i32},
    }
}
extern "system" fn emu_io(context:*mut c_void,_access:*mut c_void)->i32 {callback(context,|_|Err("Unexpected port callback during MMIO emulation".into()))}
extern "system" fn emu_get(context:*mut c_void,names:*const u32,count:u32,values:*mut Register)->i32 {
    callback(context,|ctx| {
        if names.is_null()||values.is_null()||count==0||count>256 {return Err("Invalid register callback buffers".into());}
        unsafe {check(WHvGetVirtualProcessorRegisters(ctx.partition,0,names,count,values),"MMIO get registers")}
    })
}
extern "system" fn emu_set(context:*mut c_void,names:*const u32,count:u32,values:*const Register)->i32 {
    callback(context,|ctx| {
        if names.is_null()||values.is_null()||count==0||count>256 {return Err("Invalid register callback buffers".into());}
        unsafe {check(WHvSetVirtualProcessorRegisters(ctx.partition,0,names,count,values),"MMIO set registers")}
    })
}
extern "system" fn emu_translate(context:*mut c_void,gva:u64,flags:u32,result:*mut u32,gpa:*mut u64)->i32 {
    callback(context,|ctx| {
        if result.is_null()||gpa.is_null() {return Err("Invalid translation callback outputs".into());}
        let mut translated=Translation{code:0,reserved:0};let mut address=0;
        unsafe {check(WHvTranslateGva(ctx.partition,0,gva,flags,&mut translated,&mut address),"WHvTranslateGva")?;*result=translated.code;*gpa=address;}
        ctx.translations+=1;Ok(())
    })
}
extern "system" fn emu_memory(context:*mut c_void,access:*mut EmulatorMemory)->i32 {
    callback(context,|ctx| {
        if access.is_null() {return Err("Invalid memory callback buffer".into());}
        let a=unsafe {&mut *access};let n=usize::from(a.size);
        if n==0||n>8||a.direction>1 {return Err("Unsupported emulator memory shape".into());}
        let end=a.gpa.checked_add(n as u64).ok_or("MMIO callback address overflow")?;
        if end<=ctx.bytes as u64 {
            // Single RAM allocation, bounded before pointer arithmetic; vCPU is stopped.
            let ram=unsafe {std::slice::from_raw_parts_mut(ctx.ram.add(a.gpa as usize),n)};
            if a.direction==0 {a.data[..n].copy_from_slice(ram);} else {ram.copy_from_slice(&a.data[..n]);}return Ok(());
        }
        let in_ram=end<=ctx.bytes as u64;
        let in_framebuffer=(a.gpa>=crate::framebuffer::GPA&&end<=(crate::framebuffer::GPA+crate::framebuffer::MAPPED_BYTES as u64));
        let in_mmio=(a.gpa>=MMIO_BASE&&end<=MMIO_BASE+8*4096);
        if !in_ram&&!in_framebuffer&&!in_mmio {return Err(format!("Emulator memory access outside RAM/device page at gpa=0x{:x}",a.gpa));}
        if !in_ram&&![1,2,4].contains(&a.size) {return Err("Unsupported MMIO register access width".into());}
        // Framebuffer region without a --framebuffer plan: return 0xFFFFFFFF
        // for reads, ignore writes, so a DRM-enabled kernel probing fbdev
        // memory does not crash the VM.
        if in_framebuffer {
            if a.direction==0 {a.data[..n].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes()[..n]);}
            ctx.accesses+=1;return Ok(());
        }
        let offset=if a.gpa>=MMIO_GPU_BASE {a.gpa-MMIO_GPU_BASE} else if a.gpa>=MMIO_SPI_BASE {a.gpa-MMIO_SPI_BASE} else if a.gpa>=MMIO_I2C_BASE {a.gpa-MMIO_I2C_BASE} else if a.gpa>=MMIO_GPIO_BASE {a.gpa-MMIO_GPIO_BASE} else if a.gpa>=MMIO_AUDIO_BASE {a.gpa-MMIO_AUDIO_BASE} else if a.gpa>=MMIO_NET_BASE {a.gpa-MMIO_NET_BASE} else {a.gpa-MMIO_BASE};
        if a.direction==0 {let value=ctx.device.read(offset,a.size)?.to_le_bytes();a.data[..n].copy_from_slice(&value[..n]);}
        else {
            // Defined RO registers/config ignore writes, without changing device state.
            // Config space bound differs per device: block 0x100..0x200, net MAC 0x100..0x106,
            // audio/platform controllers none. The VirtIO RO-offset filter applies only to
            // VirtIO devices; platform controllers (GPIO/I2C/SPI) own their register map and
            // reject writes to their own read-only registers via ctx.device.write.
            let (is_net,is_audio,is_platform)=(matches!(ctx.device,MmioDevice::Net(_)),matches!(ctx.device,MmioDevice::Audio(_)),matches!(ctx.device,MmioDevice::Gpio(_)|MmioDevice::I2c(_)|MmioDevice::Spi(_)));
            let config_end=if is_net {0x106u64} else if is_audio||is_platform {0u64} else {0x200u64};
            let config=(0x100..config_end).contains(&offset)&&end<=(if is_net {MMIO_NET_BASE} else if is_audio {MMIO_AUDIO_BASE} else {MMIO_BASE})+config_end&&[1,2,4].contains(&a.size);
            let readonly=!is_platform&&[0,4,8,0xc,0x10,0x34,0x60,0xfc].contains(&offset)&&a.size==4&&offset%4==0;
            if !config&&!readonly {
                let value=u32::from_le_bytes(a.data[..4].try_into().unwrap());ctx.device.write(offset,a.size,value)?;
            }
        }
        ctx.accesses+=1;Ok(())
    })
}
#[repr(C,align(8))]
struct MemoryExit([u8;40]);
struct Emulator {handle:*mut c_void,_callbacks:Box<EmulatorCallbacks>}
impl Emulator {
    fn new()->Result<Self,String> {
        let callbacks=Box::new(EmulatorCallbacks{size:std::mem::size_of::<EmulatorCallbacks>() as u32,reserved:0,io:emu_io,memory:emu_memory,get:emu_get,set:emu_set,translate:emu_translate});
        let mut handle=std::ptr::null_mut();unsafe {check(WHvEmulatorCreateEmulator(&*callbacks,&mut handle),"WHvEmulatorCreateEmulator")?;}
        if handle.is_null() {return Err("WHP emulator returned null handle".into());}Ok(Self{handle,_callbacks:callbacks})
    }
    fn emulate(&self,ctx:&mut MmioContext,exit:&ExitContext,fetch:bool)->Result<(),String> {
        let mut access=MemoryExit([0;40]);access.0.copy_from_slice(&exit.0[48..88]);
        if fetch {access.0[0]=0;access.0[4..20].fill(0);} // Exercise GVA translation and RAM fetch, not only cached instruction bytes.
        let mut status=0;
        let hr=unsafe {WHvEmulatorTryMmioEmulation(self.handle,(ctx as *mut MmioContext).cast(),exit.0[8..].as_ptr(),access.0.as_ptr(),&mut status)};
        if let Some(error)=&ctx.error {return Err(error.clone());}check(hr,"WHvEmulatorTryMmioEmulation")?;
        if status!=1 {return Err(format!("MMIO emulator did not complete instruction: status=0x{status:x}"));}Ok(())
    }
}
impl Drop for Emulator {fn drop(&mut self) {if unsafe {WHvEmulatorDestroyEmulator(self.handle)}<0 {eprintln!("NATIVE_TEARDOWN_ERROR emulator delete");}}}

struct LiveBlock {context:MmioContext,emulator:Emulator,backend:crate::overlay::Overlay}
impl LiveBlock {
    fn access(&mut self,exit:&ExitContext)->Result<(),String> {
        self.emulator.emulate(&mut self.context,exit,false)?;
        if self.context.device.work_pending() {
            // Sole vCPU stopped, synchronous emulator callbacks have all returned.
            let ram=unsafe {std::slice::from_raw_parts_mut(self.context.ram,self.context.bytes)};
            let mut memory=crate::guest_memory::GuestMemory::new(vec![crate::guest_memory::RamRegion{gpa:0,bytes:ram,writable:true}])?;
            if let MmioDevice::Block(device)=&mut self.context.device {device.service(&mut memory,&mut self.backend,256)?;}
        }
        Ok(())
    }
}

/// Live net device: same MMIO emulation pattern as the block device, but services
/// the VirtIO-net queues with a loopback backend. Activated only when a net device
/// is requested; the block boot path is unaffected.
struct LiveNet {context:MmioContext,emulator:Emulator,backend:crate::virtio_net::Loopback}
impl LiveNet {
    fn access(&mut self,exit:&ExitContext)->Result<(),String> {
        self.emulator.emulate(&mut self.context,exit,false)?;
        if self.context.device.work_pending() {
            let ram=unsafe {std::slice::from_raw_parts_mut(self.context.ram,self.context.bytes)};
            let mut memory=crate::guest_memory::GuestMemory::new(vec![crate::guest_memory::RamRegion{gpa:0,bytes:ram,writable:true}])?;
            if let MmioDevice::Net(device)=&mut self.context.device {device.service(&mut memory,&mut self.backend,256)?;}
        }
        Ok(())
    }
}

/// Live audio device: same MMIO emulation pattern, but services the VirtIO-sound
/// queues with a null backend. Activated only when an audio device is requested.
struct LiveAudio {context:MmioContext,emulator:Emulator,backend:Box<dyn crate::virtio_audio::AudioBackend>}
impl LiveAudio {
    fn access(&mut self,exit:&ExitContext)->Result<(),String> {
        self.emulator.emulate(&mut self.context,exit,false)?;
        if self.context.device.work_pending() {
            let ram=unsafe {std::slice::from_raw_parts_mut(self.context.ram,self.context.bytes)};
            let mut memory=crate::guest_memory::GuestMemory::new(vec![crate::guest_memory::RamRegion{gpa:0,bytes:ram,writable:true}])?;
            if let MmioDevice::Audio(device)=&mut self.context.device {device.service(&mut memory,&mut *self.backend,256)?;}
        }
        Ok(())
    }
}

/// Live GPIO device: MMIO emulation with no backend. The controller's registers
/// are accessed directly through the emulator callback; work_pending is always
/// false for GPIO so no service step is needed.
#[allow(dead_code)]
struct LiveGpio {context:MmioContext,emulator:Emulator}
#[allow(dead_code)]
impl LiveGpio {
    fn access(&mut self,exit:&ExitContext)->Result<(),String> {
        self.emulator.emulate(&mut self.context,exit,false)?;
        Ok(())
    }
}

/// Live I2C device: MMIO emulation with no backend. Transfers complete
/// synchronously in the register write path, so no service step is needed.
#[allow(dead_code)]
struct LiveI2c {context:MmioContext,emulator:Emulator}
#[allow(dead_code)]
impl LiveI2c {
    fn access(&mut self,exit:&ExitContext)->Result<(),String> {
        self.emulator.emulate(&mut self.context,exit,false)?;
        Ok(())
    }
}

/// Live SPI device: MMIO emulation with no backend. Transfers complete
/// synchronously in the register write path, so no service step is needed.
#[allow(dead_code)]
struct LiveSpi {context:MmioContext,emulator:Emulator}
#[allow(dead_code)]
impl LiveSpi {
    fn access(&mut self,exit:&ExitContext)->Result<(),String> {
        self.emulator.emulate(&mut self.context,exit,false)?;
        Ok(())
    }
}

/// Live VirtIO-GPU device: same MMIO emulation pattern, but services the
/// command queue with a scanout backend. Activated only when a GPU device is
/// requested; the software boot path is unaffected.
struct LiveGpu {context:MmioContext,emulator:Emulator,backend:crate::virtio_gpu::ScanoutBackend}
impl LiveGpu {
    fn access(&mut self,exit:&ExitContext)->Result<(),String> {
        self.emulator.emulate(&mut self.context,exit,false)?;
        if self.context.device.work_pending() {
            let ram=unsafe {std::slice::from_raw_parts_mut(self.context.ram,self.context.bytes)};
            let mut memory=crate::guest_memory::GuestMemory::new(vec![crate::guest_memory::RamRegion{gpa:0,bytes:ram,writable:true}])?;
            if let MmioDevice::Gpu(device)=&mut self.context.device {
                device.sync_scanout(&mut self.backend)?;
                device.service(&mut memory,&mut self.backend,256)?;
            }
        }
        Ok(())
    }
}

/// Live null device: services MMIO accesses to slots without a configured
/// VirtIO device by returning the "no device" magic (0xFFFFFFFF). This lets
/// the kernel's virtio-mmio probe fail gracefully instead of crashing the VM
/// when it scans multiple MMIO slots (e.g. virtio_mmio.device=4K@base:7).
struct LiveNull {context:MmioContext,emulator:Emulator}
impl LiveNull {
    fn access(&mut self,exit:&ExitContext)->Result<(),String> {
        self.emulator.emulate(&mut self.context,exit,false)?;
        Ok(())
    }
}

pub fn mmio_smoke()->Result<(),String> {
    use crate::cpu;
    for long in [false,true] {
        let mode=if long {cpu::CpuMode::Long64} else {cpu::CpuMode::Protected32};let mut f=cpu::fixture(mode)?;
        // Guest VA/GPA d0000000 mapped in guest page tables, but absent from host GPA mapping.
        if long {
            f.ram[0x2018..0x2020].copy_from_slice(&0x7003u64.to_le_bytes());
            f.ram[0x7400..0x7408].copy_from_slice(&(MMIO_BASE|0x83).to_le_bytes());
        }
        // Three requests: write sector 1, flush, then read sector 1 into a distinct buffer.
        // Rings are fixture input; guest code itself negotiates, starts and notifies the queue.
        for (index,address,length,flags,next) in [
            (0,0x22000u64,16u32,1u16,1u16),(1,0x23000,512,1,2),(2,0x24000,1,2,0),
            (3,0x22010,16,1,4),(4,0x24001,1,2,0),
            (5,0x22020,16,1,6),(6,0x23200,512,3,7),(7,0x24002,1,2,0),
        ] {
            let at=0x20000+index*16;
            f.ram[at..at+8].copy_from_slice(&address.to_le_bytes());
            f.ram[at+8..at+12].copy_from_slice(&length.to_le_bytes());
            f.ram[at+12..at+14].copy_from_slice(&flags.to_le_bytes());
            f.ram[at+14..at+16].copy_from_slice(&next.to_le_bytes());
        }
        for (at,value) in [(0x21002,3u16),(0x21004,0),(0x21006,3),(0x21008,5)] {f.ram[at..at+2].copy_from_slice(&value.to_le_bytes());}
        f.ram[0x22000..0x22004].copy_from_slice(&1u32.to_le_bytes());
        f.ram[0x22008..0x22010].copy_from_slice(&1u64.to_le_bytes());
        f.ram[0x22010..0x22014].copy_from_slice(&4u32.to_le_bytes());
        f.ram[0x22028..0x22030].copy_from_slice(&1u64.to_le_bytes());
        for (n,byte) in f.ram[0x23000..0x23200].iter_mut().enumerate() {*byte=(n as u8).wrapping_mul(37).wrapping_add(9);}
        f.ram[0x24000..0x24003].fill(0xff);
        if !long {
            f.idt_limit=0x12f;
            f.ram[cpu::IDT+0x128..cpu::IDT+0x130].copy_from_slice(&[0,0x90,8,0,0,0x8e,0,0]);
            // Preserve EAX; capture IRQ status; ACK device before PIC EOI; IRETD.
            let handler=[0x50,0xa1,0x60,0,0,0xd0,0xa3,0x30,0xb0,1,0,
                0xb8,1,0,0,0,0xa3,0x64,0,0,0xd0,0xff,0x05,0x34,0xb0,1,0,
                0xb0,0x20,0xe6,0x20,0x58,0xcf];
            f.ram[0x9000..0x9000+handler.len()].copy_from_slice(&handler);
        }
        let mut code=Vec::new();
        let load=|code:&mut Vec<u8>,offset:u32,size:u8,dest:u32| {
            if long {code.push(0x67);}code.push(if size==1 {0xa0} else {0xa1});code.extend_from_slice(&((MMIO_BASE as u32)+offset).to_le_bytes());
            if size==1 {code.extend_from_slice(&[0x0f,0xb6,0xc0]);}
            if long {code.push(0x67);}code.push(0xa3);code.extend_from_slice(&dest.to_le_bytes());
        };
        let write=|code:&mut Vec<u8>,offset:u32,value:u32| {
            code.push(0xb8);code.extend_from_slice(&value.to_le_bytes());if long {code.push(0x67);}code.push(0xa3);code.extend_from_slice(&((MMIO_BASE as u32)+offset).to_le_bytes());
        };
        for (index,offset) in [0,4,8,0x10,0x108,0x10c].into_iter().enumerate() {load(&mut code,offset,4,0x1b000+index as u32*4);}
        write(&mut code,0,0);load(&mut code,0,4,0x1b018); // RO write must be ignored.
        for (offset,value) in [(0x70,1),(0x70,3),(0x24,0),(0x20,518),(0x24,1),(0x20,1),(0x70,11)] {write(&mut code,offset,value);}
        load(&mut code,0x70,4,0x1b01c);load(&mut code,0x108,1,0x1b020);
        if !long {code.extend_from_slice(&[0xfb,0x90]);}
        for (offset,value) in [(0x30,0),(0x38,8),(0x80,0x20000),(0x90,0x21000),(0xa0,0x21800),(0x44,1),(0x70,15),(0x50,0)] {write(&mut code,offset,value);}
        load(&mut code,0x60,4,0x1b028);write(&mut code,0x64,1);load(&mut code,0x60,4,0x1b02c);
        write(&mut code,0x70,0);load(&mut code,0x70,4,0x1b024);code.push(0xf4);
        // Execute long-mode code through the nonidentity VA 400000 -> PA 18000 alias.
        let physical=if long {cpu::DATA} else {cpu::ENTRY};f.ram[physical..physical+code.len()].copy_from_slice(&code);
        with_guest_at(&[],0,cpu::RAM_BYTES,|partition,memory| {
            unsafe {std::ptr::copy_nonoverlapping(f.ram.as_ptr(),memory.address.cast::<u8>(),f.ram.len());}
            configure_fixture(partition,&f)?;if long {set_registers(partition,&[0x10],&[Register::scalar(cpu::ALIAS)])?;}
            let root=std::env::temp_dir().join(format!("nexus-live-mmio-{}-{}",std::process::id(),long));
            std::fs::create_dir(&root).map_err(|e|e.to_string())?;
            let raw=root.join("raw");let base=root.join("base.zlag");let cow=root.join("disk.cow");
            std::fs::write(&raw,[0u8;2048]).map_err(|e|e.to_string())?;
            crate::image::import_raw(&raw,&base).map_err(|e|e.to_string())?;
            let original=std::fs::read(&base).map_err(|e|e.to_string())?;
            let mut backend=crate::overlay::Overlay::create(&base,&cow)?;
            let mut serviced=0;let mut injections=0;
            let mut platform=crate::platform::EarlyPlatform::default();
            if !long {
                for (port,value) in [(0x20,0x11),(0x21,0x20),(0x21,4),(0x21,1),(0x21,0xdf),(0x4d0,0x20)] {platform.pic.access(port,true,value)?;}
            }
            let emulator=Emulator::new()?;let mut ctx=MmioContext{partition:partition.0,ram:memory.address.cast(),bytes:memory.bytes,device:MmioDevice::Block(crate::virtio_mmio::BlockMmio::new(4,false)?),error:None,translations:0,accesses:0};
            let (result,timed_out)=bounded(partition,5000,|| {
                for _ in 0..128 {
                    if !long {
                        platform.pic.set_irq5(ctx.device.irq_asserted());
                        if queue_interrupt(partition,&mut platform)?.is_some() {injections+=1;}
                    }
                    let exit=run_once(partition)?;
                    match exit.reason() {8=>return Ok(Ok(())),1=>{
                        emulator.emulate(&mut ctx,&exit,long)?;
                        if ctx.device.work_pending() {
                            // Emulator returned and sole vCPU is stopped. No callback/RAM borrow is live.
                            let ram=unsafe {std::slice::from_raw_parts_mut(ctx.ram,ctx.bytes)};
                            let mut memory=crate::guest_memory::GuestMemory::new(vec![crate::guest_memory::RamRegion{gpa:0,bytes:ram,writable:true}])?;
                            let done=if let MmioDevice::Block(device)=&mut ctx.device {device.service(&mut memory,&mut backend,8)?} else {return Err("MMIO smoke context is not a block device".into())};
                            if done.iter().any(|c|c.status!=0) {return Err("Live MMIO block request failed".into());}
                            serviced+=done.len();
                        }
                    },2=>{
                        let io=crate::io::IoExit::decode(&exit.0)?;
                        if io.port!=0x20||!io.write||io.size!=1 {return Err("Unexpected MMIO fixture port access".into());}
                        let mode=crate::cpu::CpuMode::from_exit(&exit.0)?;
                        platform.pic.set_irq5(ctx.device.irq_asserted());
                        platform.access(io,0)?;
                        set_registers(partition,&[0x10],&[Register::scalar(mode.next_rip(io.rip,io.instruction_len)?)])?;
                    },reason=>return Err(format!("Unexpected MMIO fixture exit 0x{reason:x}"))}
                }Ok(Err("MMIO fixture exit budget exceeded".to_string()))
            })?;
            if timed_out {return Err("MMIO fixture deadline exceeded".into());}result?;
            let bytes=unsafe {std::slice::from_raw_parts(memory.address.cast::<u8>().add(0x1b000),48)};
            for (n,expected) in [0x74726976,2,2,518,131072,1,0x74726976,11,0,0,u32::from(long),0].into_iter().enumerate() {
                let actual=u32::from_le_bytes(bytes[n*4..n*4+4].try_into().unwrap());if actual!=expected {return Err(format!("MMIO register {n} mismatch 0x{actual:x}"));}
            }
            if !long {
                let captured=unsafe {std::slice::from_raw_parts(ctx.ram.add(0x1b030),8)};
                if captured!=[1,0,0,0,1,0,0,0]||injections!=1||platform.pic.pending_vector().is_some()||scalar_register(partition,4)?!=cpu::STACK {
                    return Err("VirtIO external IRQ5 delivery/ACK/EOI/IRETD contract failed".into());
                }
            }
            if serviced!=3 {return Err("Missing live write/flush/read completions".into());}
            let ram=unsafe {std::slice::from_raw_parts(ctx.ram,ctx.bytes)};
            if ram[0x23000..0x23200]!=ram[0x23200..0x23400]||ram[0x24000..0x24003]!=[0;3]||ram[0x21802..0x21804]!=3u16.to_le_bytes() {
                return Err("Live MMIO DMA data/status/used index mismatch".into());
            }
            drop(backend);
            let mut reopened=crate::overlay::Overlay::open(&base,&cow)?;
            let mut persisted=[0;512];reopened.read_sectors(1,&mut persisted)?;
            if persisted!=ram[0x23000..0x23200]||std::fs::read(&base).map_err(|e|e.to_string())?!=original {return Err("Live MMIO COW persistence/base integrity mismatch".into());}
            drop(reopened);std::fs::remove_dir_all(&root).map_err(|e|e.to_string())?;
            if ctx.accesses<10||(long&&ctx.translations==0) {return Err("Missing real MMIO/translation callback execution".into());}Ok(())
        })?;
    }Ok(())
}

/// Live WHP MMIO smoke for the VirtIO-net device: same emulator-callback path as
/// `mmio_smoke`, but routes net-base GPAs to a `LiveNet` with a loopback backend.
/// Proves the enum MMIO dispatch, GPA routing, IRQ6 assertion and a full
/// TX→loopback→RX round-trip through the real WHP emulator on a Windows host.
pub fn net_mmio_smoke()->Result<(),String> {
    use crate::cpu;
    for long in [false,true] {
        let mode=if long {cpu::CpuMode::Long64} else {cpu::CpuMode::Protected32};let mut f=cpu::fixture(mode)?;
        // Map the net MMIO base in guest page tables (long mode only).
        if long {
            f.ram[0x2018..0x2020].copy_from_slice(&0x7003u64.to_le_bytes());
            f.ram[0x7400..0x7408].copy_from_slice(&(MMIO_NET_BASE|0x83).to_le_bytes());
        }
        // Fixture virtqueues: TX (sel=1) descriptor table 0x20000, avail 0x20100, used 0x20200;
        // RX (sel=0) descriptor table 0x20500, avail 0x20600, used 0x20700.
        // TX packet at 0x20300 (12-byte header + 5-byte payload), TX status at 0x20400.
        // RX buffer at 0x20800 (64 bytes).
        // TX descriptor chain: [packet (RO, NEXT->1), status (WR)].
        let tx_packet_addr=0x20300u64;let tx_status_addr=0x20400u64;
        let descs=[(0x20000u64,tx_packet_addr,17u32,1u16,1u16),(0x20010,tx_status_addr,1,2,0)];
        for (base,address,length,flags,next) in descs {
            let b=base as usize;
            f.ram[b..b+8].copy_from_slice(&address.to_le_bytes());
            f.ram[b+8..b+12].copy_from_slice(&length.to_le_bytes());
            f.ram[b+12..b+14].copy_from_slice(&flags.to_le_bytes());
            f.ram[b+14..b+16].copy_from_slice(&next.to_le_bytes());
        }
        // TX avail ring: flags=0, index=1, head=0. TX used ring: index=0, head=0.
        f.ram[0x20100..0x20102].copy_from_slice(&0u16.to_le_bytes());
        f.ram[0x20102..0x20104].copy_from_slice(&1u16.to_le_bytes());
        f.ram[0x20104..0x2010c].copy_from_slice(&0u64.to_le_bytes());
        f.ram[0x20200..0x20202].copy_from_slice(&0u16.to_le_bytes());
        f.ram[0x20202..0x2020a].copy_from_slice(&0u64.to_le_bytes());
        // TX packet: 12-byte header (dst+src MAC) + "NEXUS".
        let header=crate::virtio_net::net_header();
        f.ram[0x20300..0x2030c].copy_from_slice(&header);
        f.ram[0x2030c..0x20311].copy_from_slice(b"NEXUS");
        // RX descriptor: single writable buffer at 0x20800, len 64.
        f.ram[0x20500..0x20508].copy_from_slice(&0x20800u64.to_le_bytes());
        f.ram[0x20508..0x2050c].copy_from_slice(&64u32.to_le_bytes());
        f.ram[0x2050c..0x2050e].copy_from_slice(&2u16.to_le_bytes());
        f.ram[0x2050e..0x20510].copy_from_slice(&0u16.to_le_bytes());
        // RX avail ring: flags=0, index=1, head=0. RX used ring: index=0, head=0.
        f.ram[0x20600..0x20602].copy_from_slice(&0u16.to_le_bytes());
        f.ram[0x20602..0x20604].copy_from_slice(&1u16.to_le_bytes());
        f.ram[0x20604..0x2060c].copy_from_slice(&0u64.to_le_bytes());
        f.ram[0x20700..0x20702].copy_from_slice(&0u16.to_le_bytes());
        f.ram[0x20702..0x2070a].copy_from_slice(&0u64.to_le_bytes());
        let mut code=Vec::new();
        let load=|code:&mut Vec<u8>,offset:u32,size:u8,dest:u32| {
            if long {code.push(0x67);}code.push(if size==1 {0xa0} else {0xa1});code.extend_from_slice(&((MMIO_NET_BASE as u32)+offset).to_le_bytes());
            if size==1 {code.extend_from_slice(&[0x0f,0xb6,0xc0]);}
            if long {code.push(0x67);}code.push(0xa3);code.extend_from_slice(&dest.to_le_bytes());
        };
        let write=|code:&mut Vec<u8>,offset:u32,value:u32| {
            code.push(0xb8);code.extend_from_slice(&value.to_le_bytes());if long {code.push(0x67);}code.push(0xa3);code.extend_from_slice(&((MMIO_NET_BASE as u32)+offset).to_le_bytes());
        };
        // Read device identity: device ID (0), version (4), vendor ID (0xc), queue num (0x34).
        for (index,offset) in [0,4,0xc,0x34].into_iter().enumerate() {load(&mut code,offset,4,0x1b000+index as u32*4);}
        // Negotiate: ACK, DRIVER, features (F_MAC low, VERSION_1 high), FEATURES_OK, DRIVER_OK.
        for (offset,value) in [(0x70,1),(0x70,3),(0x24,0),(0x20,1),(0x24,1),(0x20,1),(0x70,11),(0x70,15)] {write(&mut code,offset,value);}
        load(&mut code,0x70,4,0x1b010); // status after DRIVER_OK.
        // TX queue setup (sel=1) + notify.
        for (offset,value) in [(0x30,1),(0x38,8),(0x80,0x20000),(0x90,0x20100),(0xa0,0x20200),(0x44,1),(0x50,0)] {write(&mut code,offset,value);}
        // RX queue setup (sel=0) + notify.
        for (offset,value) in [(0x30,0),(0x38,8),(0x80,0x20500),(0x90,0x20600),(0xa0,0x20700),(0x44,1),(0x50,0)] {write(&mut code,offset,value);}
        // Interrupt status + ACK + re-read.
        load(&mut code,0x60,4,0x1b014);write(&mut code,0x64,1);load(&mut code,0x60,4,0x1b018);
        // Reset + read status.
        write(&mut code,0x70,0);load(&mut code,0x70,4,0x1b01c);
        code.push(0xf4); // HLT
        let physical=if long {cpu::DATA} else {cpu::ENTRY};f.ram[physical..physical+code.len()].copy_from_slice(&code);
        with_guest_at(&[],0,cpu::RAM_BYTES,|partition,memory| {
            unsafe {std::ptr::copy_nonoverlapping(f.ram.as_ptr(),memory.address.cast::<u8>(),f.ram.len());}
            configure_fixture(partition,&f)?;if long {set_registers(partition,&[0x10],&[Register::scalar(cpu::ALIAS)])?;}
            let emulator=Emulator::new()?;
            let mut ctx=MmioContext{partition:partition.0,ram:memory.address.cast(),bytes:memory.bytes,device:MmioDevice::Net(crate::virtio_net_mmio::NetMmio::new()),error:None,translations:0,accesses:0};
            let mut backend=crate::virtio_net::Loopback::new();
            let mut serviced_tx=0;let mut irq_seen=false;
            let mut platform=crate::platform::EarlyPlatform::default();
            let (result,timed_out)=bounded(partition,5000,|| {
                for _ in 0..128 {
                    platform.pic.set_irq6(ctx.device.irq_asserted());
                    if ctx.device.irq_asserted() {irq_seen=true;}
                    let exit=run_once(partition)?;
                    match exit.reason() {
                        8=>return Ok(Ok(())),
                        1=>{
                            emulator.emulate(&mut ctx,&exit,long)?;
                            if ctx.device.work_pending() {
                                let ram=unsafe {std::slice::from_raw_parts_mut(ctx.ram,ctx.bytes)};
                                let mut memory=crate::guest_memory::GuestMemory::new(vec![crate::guest_memory::RamRegion{gpa:0,bytes:ram,writable:true}])?;
                                let done=if let MmioDevice::Net(device)=&mut ctx.device {device.service(&mut memory,&mut backend,8)?} else {return Err("Net MMIO smoke context is not a net device".into())};
                                serviced_tx+=done.len();
                            }
                        },
                        2=>{
                            let io=crate::io::IoExit::decode(&exit.0)?;
                            if io.port!=0x20||!io.write||io.size!=1 {return Err("Unexpected net MMIO fixture port access".into());}
                            let mode=crate::cpu::CpuMode::from_exit(&exit.0)?;
                            platform.pic.set_irq6(ctx.device.irq_asserted());
                            platform.access(io,0)?;
                            set_registers(partition,&[0x10],&[Register::scalar(mode.next_rip(io.rip,io.instruction_len)?)])?;
                        },
                        reason=>return Err(format!("Unexpected net MMIO fixture exit 0x{reason:x}"))
                    }
                }Ok(Err("Net MMIO fixture exit budget exceeded".to_string()))
            })?;
            if timed_out {return Err("Net MMIO fixture deadline exceeded".into());}result?;
            // Verify register reads.
            let bytes=unsafe {std::slice::from_raw_parts(memory.address.cast::<u8>().add(0x1b000),32)};
            for (n,expected) in [0x74726976u32,2,0x5358454e,256,15].into_iter().enumerate() {
                let actual=u32::from_le_bytes(bytes[n*4..n*4+4].try_into().unwrap());if actual!=expected {return Err(format!("Net MMIO register {n} mismatch 0x{actual:x}"));}
            }
            // Verify interrupt was asserted and cleared by ACK.
            let irq_before=u32::from_le_bytes(bytes[5*4..5*4+4].try_into().unwrap());
            let irq_after=u32::from_le_bytes(bytes[6*4..6*4+4].try_into().unwrap());
            if irq_before==0||irq_after!=0 {return Err("Net MMIO interrupt status/ACK mismatch".into());}
            // Verify status after reset.
            let status_reset=u32::from_le_bytes(bytes[7*4..7*4+4].try_into().unwrap());
            if status_reset!=0 {return Err("Net MMIO status not cleared by reset".into());}
            // Verify TX completion: status descriptor written with 0.
            let tx_status=u32::from_le_bytes(unsafe {std::slice::from_raw_parts(ctx.ram.add(0x20400),4)}.try_into().unwrap());
            if tx_status!=0 {return Err("Net MMIO TX completion status not success".into());}
            // Verify RX completion: RX buffer contains the loopback frame.
            let rx=unsafe {std::slice::from_raw_parts(ctx.ram.add(0x20800),17)};
            if rx[..12]!=header||&rx[12..]!=b"NEXUS" {return Err("Net MMIO RX loopback frame data mismatch".into());}
            // Verify used-ring indices advanced. The used index lives at used+2.
            let tx_used_idx=u16::from_le_bytes(unsafe {std::slice::from_raw_parts(ctx.ram.add(0x20202),2)}.try_into().unwrap());
            let rx_used_idx=u16::from_le_bytes(unsafe {std::slice::from_raw_parts(ctx.ram.add(0x20702),2)}.try_into().unwrap());
            if tx_used_idx!=1||rx_used_idx!=1 {return Err("Net MMIO used-ring index mismatch".into());}
            if !irq_seen {return Err("Net MMIO IRQ6 was never asserted".into());}
            if serviced_tx<2 {return Err("Net MMIO did not complete both TX and RX requests".into());}
            if ctx.accesses<10||(long&&ctx.translations==0) {return Err("Missing real net MMIO/translation callback execution".into());}
            Ok(())
        })?;
    }Ok(())
}

/// Live MMIO smoke for the VirtIO-MMIO audio device: runs the real emulator
/// CPU executing guest code that maps the audio MMIO base, negotiates features,
/// sets up playback (TX) and capture (RX) queues, notifies both, then services
/// the device from the MMIO callback. Verifies register reads, IRQ7, completion
/// status, capture silence delivery, used-ring indices, and real callback
/// execution. Null backend only; no host audio stack or guest boot.
pub fn audio_mmio_smoke()->Result<(),String> {
    use crate::cpu;
    for long in [false,true] {
        let mode=if long {cpu::CpuMode::Long64} else {cpu::CpuMode::Protected32};let mut f=cpu::fixture(mode)?;
        // Map the audio MMIO base in guest page tables (long mode only).
        // Use MMIO_BASE (0xd0000000, 2MB-aligned) as the 2MB page GPA so VA
        // 0xd0002000 maps to GPA 0xd0002000 (identity within the 2MB page),
        // landing in the MMIO_AUDIO_BASE range for the emulator callback.
        if long {
            f.ram[0x2018..0x2020].copy_from_slice(&0x7003u64.to_le_bytes());
            f.ram[0x7400..0x7408].copy_from_slice(&(MMIO_BASE|0x83).to_le_bytes());
        }
        // Fixture virtqueues: TX (sel=1, playback) descriptor table 0x20000, avail 0x20100, used 0x20200;
        // RX (sel=0, capture) descriptor table 0x20500, avail 0x20600, used 0x20700.
        // TX samples at 0x20300 (32 bytes), TX status byte at 0x20400.
        // RX capture buffer at 0x20800 (256 bytes).
        // TX descriptor chain: [samples (RO, NEXT->1), status (WR, 1 byte)].
        let tx_samples_addr=0x20300u64;let tx_status_addr=0x20400u64;
        let tx_descs=[(0x20000u64,tx_samples_addr,32u32,1u16,1u16),(0x20010,tx_status_addr,1,2,0)];
        for (base,address,length,flags,next) in tx_descs {
            let b=base as usize;
            f.ram[b..b+8].copy_from_slice(&address.to_le_bytes());
            f.ram[b+8..b+12].copy_from_slice(&length.to_le_bytes());
            f.ram[b+12..b+14].copy_from_slice(&flags.to_le_bytes());
            f.ram[b+14..b+16].copy_from_slice(&next.to_le_bytes());
        }
        // TX avail ring: flags=0, index=1, head=0. TX used ring: index=0, head=0.
        f.ram[0x20100..0x20102].copy_from_slice(&0u16.to_le_bytes());
        f.ram[0x20102..0x20104].copy_from_slice(&1u16.to_le_bytes());
        f.ram[0x20104..0x2010c].copy_from_slice(&0u64.to_le_bytes());
        f.ram[0x20200..0x20202].copy_from_slice(&0u16.to_le_bytes());
        f.ram[0x20202..0x2020a].copy_from_slice(&0u64.to_le_bytes());
        // TX playback samples.
        for n in 0..32u32 {f.ram[0x20300+n as usize]=(n.wrapping_mul(13) as u8).to_le();}
        // RX descriptor: single writable capture buffer at 0x20800, len 256.
        f.ram[0x20500..0x20508].copy_from_slice(&0x20800u64.to_le_bytes());
        f.ram[0x20508..0x2050c].copy_from_slice(&256u32.to_le_bytes());
        f.ram[0x2050c..0x2050e].copy_from_slice(&2u16.to_le_bytes());
        f.ram[0x2050e..0x20510].copy_from_slice(&0u16.to_le_bytes());
        // RX avail ring: flags=0, index=1, head=0. RX used ring: index=0, head=0.
        f.ram[0x20600..0x20602].copy_from_slice(&0u16.to_le_bytes());
        f.ram[0x20602..0x20604].copy_from_slice(&1u16.to_le_bytes());
        f.ram[0x20604..0x2060c].copy_from_slice(&0u64.to_le_bytes());
        f.ram[0x20700..0x20702].copy_from_slice(&0u16.to_le_bytes());
        f.ram[0x20702..0x2070a].copy_from_slice(&0u64.to_le_bytes());
        let mut code=Vec::new();
        let load=|code:&mut Vec<u8>,offset:u32,size:u8,dest:u32| {
            if long {code.push(0x67);}code.push(if size==1 {0xa0} else {0xa1});code.extend_from_slice(&((MMIO_AUDIO_BASE as u32)+offset).to_le_bytes());
            if size==1 {code.extend_from_slice(&[0x0f,0xb6,0xc0]);}
            if long {code.push(0x67);}code.push(0xa3);code.extend_from_slice(&dest.to_le_bytes());
        };
        let write=|code:&mut Vec<u8>,offset:u32,value:u32| {
            code.push(0xb8);code.extend_from_slice(&value.to_le_bytes());if long {code.push(0x67);}code.push(0xa3);code.extend_from_slice(&((MMIO_AUDIO_BASE as u32)+offset).to_le_bytes());
        };
        // Read device identity: device ID (0), version (4), vendor ID (0xc), queue num (0x34).
        for (index,offset) in [0,4,0xc,0x34].into_iter().enumerate() {load(&mut code,offset,4,0x1b000+index as u32*4);}
        // Negotiate: ACK, DRIVER, features (F_PCM low, VERSION_1 high), FEATURES_OK, DRIVER_OK.
        for (offset,value) in [(0x70,1),(0x70,3),(0x24,0),(0x20,1),(0x24,1),(0x20,1),(0x70,11),(0x70,15)] {write(&mut code,offset,value);}
        load(&mut code,0x70,4,0x1b010); // status after DRIVER_OK.
        // TX queue setup (sel=1) + notify.
        for (offset,value) in [(0x30,1),(0x38,8),(0x80,0x20000),(0x90,0x20100),(0xa0,0x20200),(0x44,1),(0x50,0)] {write(&mut code,offset,value);}
        // RX queue setup (sel=0) + notify.
        for (offset,value) in [(0x30,0),(0x38,8),(0x80,0x20500),(0x90,0x20600),(0xa0,0x20700),(0x44,1),(0x50,0)] {write(&mut code,offset,value);}
        // Interrupt status + ACK + re-read.
        load(&mut code,0x60,4,0x1b014);write(&mut code,0x64,1);load(&mut code,0x60,4,0x1b018);
        // Reset + read status.
        write(&mut code,0x70,0);load(&mut code,0x70,4,0x1b01c);
        code.push(0xf4); // HLT
        let physical=if long {cpu::DATA} else {cpu::ENTRY};f.ram[physical..physical+code.len()].copy_from_slice(&code);
        with_guest_at(&[],0,cpu::RAM_BYTES,|partition,memory| {
            unsafe {std::ptr::copy_nonoverlapping(f.ram.as_ptr(),memory.address.cast::<u8>(),f.ram.len());}
            configure_fixture(partition,&f)?;if long {set_registers(partition,&[0x10],&[Register::scalar(cpu::ALIAS)])?;}
            let emulator=Emulator::new()?;
            let mut ctx=MmioContext{partition:partition.0,ram:memory.address.cast(),bytes:memory.bytes,device:MmioDevice::Audio(crate::virtio_audio_mmio::AudioMmio::new()),error:None,translations:0,accesses:0};
            let mut backend=crate::virtio_audio::NullBackend::new();
            let mut serviced=0;let mut irq_seen=false;
            let mut platform=crate::platform::EarlyPlatform::default();
            let (result,timed_out)=bounded(partition,5000,|| {
                for _ in 0..128 {
                    platform.pic.set_irq7(ctx.device.irq_asserted());
                    if ctx.device.irq_asserted() {irq_seen=true;}
                    let exit=run_once(partition)?;
                    match exit.reason() {
                        8=>return Ok(Ok(())),
                        1=>{
                            emulator.emulate(&mut ctx,&exit,long)?;
                            if ctx.device.work_pending() {
                                let ram=unsafe {std::slice::from_raw_parts_mut(ctx.ram,ctx.bytes)};
                                let mut memory=crate::guest_memory::GuestMemory::new(vec![crate::guest_memory::RamRegion{gpa:0,bytes:ram,writable:true}])?;
                                let done=if let MmioDevice::Audio(device)=&mut ctx.device {device.service(&mut memory,&mut backend,8)?} else {return Err("Audio MMIO smoke context is not an audio device".into())};
                                serviced+=done.len();
                            }
                        },
                        2=>{
                            let io=crate::io::IoExit::decode(&exit.0)?;
                            if io.port!=0x20||!io.write||io.size!=1 {return Err("Unexpected audio MMIO fixture port access".into());}
                            let mode=crate::cpu::CpuMode::from_exit(&exit.0)?;
                            platform.pic.set_irq7(ctx.device.irq_asserted());
                            platform.access(io,0)?;
                            set_registers(partition,&[0x10],&[Register::scalar(mode.next_rip(io.rip,io.instruction_len)?)])?;
                        },
                        reason=>return Err(format!("Unexpected audio MMIO fixture exit 0x{reason:x}"))
                    }
                }Ok(Err("Audio MMIO fixture exit budget exceeded".to_string()))
            })?;
            if timed_out {return Err("Audio MMIO fixture deadline exceeded".into());}result?;
            // Verify register reads.
            let bytes=unsafe {std::slice::from_raw_parts(memory.address.cast::<u8>().add(0x1b000),32)};
            for (n,expected) in [0x74726976u32,2,0x5358454e,256,15].into_iter().enumerate() {
                let actual=u32::from_le_bytes(bytes[n*4..n*4+4].try_into().unwrap());if actual!=expected {return Err(format!("Audio MMIO register {n} mismatch 0x{actual:x}"));}
            }
            // Verify interrupt was asserted and cleared by ACK.
            let irq_before=u32::from_le_bytes(bytes[5*4..5*4+4].try_into().unwrap());
            let irq_after=u32::from_le_bytes(bytes[6*4..6*4+4].try_into().unwrap());
            if irq_before==0||irq_after!=0 {return Err("Audio MMIO interrupt status/ACK mismatch".into());}
            // Verify status after reset.
            let status_reset=u32::from_le_bytes(bytes[7*4..7*4+4].try_into().unwrap());
            if status_reset!=0 {return Err("Audio MMIO status not cleared by reset".into());}
            // Verify TX completion: status byte written with 0.
            let tx_status=u8::from_le(unsafe {std::slice::from_raw_parts(ctx.ram.add(0x20400),1)}[0]);
            if tx_status!=0 {return Err("Audio MMIO TX completion status not success".into());}
            // Verify RX completion: capture buffer filled with silence (null backend).
            let rx=unsafe {std::slice::from_raw_parts(ctx.ram.add(0x20800),256)};
            if !rx.iter().all(|&b| b==0) {return Err("Audio MMIO RX capture buffer not silence".into());}
            // Verify used-ring indices advanced. The used index lives at used+2.
            let tx_used_idx=u16::from_le_bytes(unsafe {std::slice::from_raw_parts(ctx.ram.add(0x20202),2)}.try_into().unwrap());
            let rx_used_idx=u16::from_le_bytes(unsafe {std::slice::from_raw_parts(ctx.ram.add(0x20702),2)}.try_into().unwrap());
            if tx_used_idx!=1||rx_used_idx!=1 {return Err("Audio MMIO used-ring index mismatch".into());}
            if !irq_seen {return Err("Audio MMIO IRQ7 was never asserted".into());}
            if serviced<2 {return Err("Audio MMIO did not complete both TX and RX requests".into());}
            if ctx.accesses<10||(long&&ctx.translations==0) {return Err("Missing real audio MMIO/translation callback execution".into());}
            Ok(())
        })?;
    }Ok(())
}

/// Live MMIO smoke for the VirtIO-MMIO GPU device: runs the real emulator CPU
/// executing guest code that maps the GPU MMIO base, negotiates features, writes
/// the scanout geometry through the config space, sets up and notifies the command
/// queue, then services the device from the MMIO callback. The pre-staged command
/// is a FILL_2D that rasterizes a red rectangle into the scanout surface. Verifies
/// register reads, scanout geometry, command completion status, IRQ assertion,
/// used-ring advance, real rasterized pixels, and real callback execution.
pub fn gpu_mmio_smoke()->Result<(),String> {
    use crate::cpu;
    use crate::virtio_gpu::GpuBackend;
    for long in [false,true] {
        let mode=if long {cpu::CpuMode::Long64} else {cpu::CpuMode::Protected32};let mut f=cpu::fixture(mode)?;
        // Map the GPU MMIO base in guest page tables (long mode only).
        // Use identity mapping (MMIO_BASE|0x83) so VA 0xd0006000 -> GPA 0xd0006000,
        // landing in the MMIO_GPU_BASE range for the emulator callback.
        if long {
            f.ram[0x2018..0x2020].copy_from_slice(&0x7003u64.to_le_bytes());
            f.ram[0x7400..0x7408].copy_from_slice(&(MMIO_BASE|0x83).to_le_bytes());
        }
        // Fixture command queue (sel=0): descriptor table 0x20000, avail 0x20100, used 0x20200.
        // Command header descriptor: [header (RO, 24 bytes, NEXT->1), status (WR, 1 byte)].
        // Header at 0x20300 (FILL_2D: opcode=4, color=red, x=0, y=0, w=4, h=4).
        // Status byte at 0x20400.
        let header_addr=0x20300u64;let status_addr=0x20400u64;
        let descs=[(0x20000u64,header_addr,24u32,1u16,1u16),(0x20010,status_addr,1,2,0)];
        for (base,address,length,flags,next) in descs {
            let b=base as usize;
            f.ram[b..b+8].copy_from_slice(&address.to_le_bytes());
            f.ram[b+8..b+12].copy_from_slice(&length.to_le_bytes());
            f.ram[b+12..b+14].copy_from_slice(&flags.to_le_bytes());
            f.ram[b+14..b+16].copy_from_slice(&next.to_le_bytes());
        }
        // FILL_2D command header: opcode=4, color=0x00ff0000, x=0, y=0, width=4, height=4.
        f.ram[0x20300..0x20304].copy_from_slice(&4u32.to_le_bytes());   // opcode FILL_2D
        f.ram[0x20304..0x20308].copy_from_slice(&0x00ff0000u32.to_le_bytes()); // color red
        f.ram[0x20308..0x2030c].copy_from_slice(&0u32.to_le_bytes());   // x
        f.ram[0x2030c..0x20310].copy_from_slice(&0u32.to_le_bytes());   // y
        f.ram[0x20310..0x20314].copy_from_slice(&4u32.to_le_bytes());   // width
        f.ram[0x20314..0x20318].copy_from_slice(&4u32.to_le_bytes());   // height
        // Avail ring: flags=0, index=1, head=0. Used ring: flags=0, index=0, head=0.
        f.ram[0x20100..0x20102].copy_from_slice(&0u16.to_le_bytes());
        f.ram[0x20102..0x20104].copy_from_slice(&1u16.to_le_bytes());
        f.ram[0x20104..0x2010c].copy_from_slice(&0u64.to_le_bytes());
        f.ram[0x20200..0x20202].copy_from_slice(&0u16.to_le_bytes());
        f.ram[0x20202..0x2020a].copy_from_slice(&0u64.to_le_bytes());
        let mut code=Vec::new();
        let load=|code:&mut Vec<u8>,offset:u32,size:u8,dest:u32| {
            if long {code.push(0x67);}code.push(if size==1 {0xa0} else {0xa1});code.extend_from_slice(&((MMIO_GPU_BASE as u32)+offset).to_le_bytes());
            if size==1 {code.extend_from_slice(&[0x0f,0xb6,0xc0]);}
            if long {code.push(0x67);}code.push(0xa3);code.extend_from_slice(&dest.to_le_bytes());
        };
        let write=|code:&mut Vec<u8>,offset:u32,value:u32| {
            code.push(0xb8);code.extend_from_slice(&value.to_le_bytes());if long {code.push(0x67);}code.push(0xa3);code.extend_from_slice(&((MMIO_GPU_BASE as u32)+offset).to_le_bytes());
        };
        // Read device identity: device ID (8), version (4), vendor ID (0xc), queue num (0x34).
        for (index,offset) in [8,4,0xc,0x34].into_iter().enumerate() {load(&mut code,offset,4,0x1b000+index as u32*4);}
        // Negotiate: ACK, DRIVER, features (F_FORMAT_LIST low = 1), FEATURES_OK, DRIVER_OK.
        for (offset,value) in [(0x70,1),(0x70,3),(0x24,0),(0x20,1),(0x24,1),(0x20,1),(0x70,11),(0x70,15)] {write(&mut code,offset,value);}
        load(&mut code,0x70,4,0x1b010); // status after DRIVER_OK.
        // Write scanout geometry through the config space: 8x8 XRGB8888, pitch 32.
        for (offset,value) in [(0x100,8),(0x104,8),(0x108,3),(0x10c,32)] {write(&mut code,offset,value);}
        for (offset,dest) in [(0x100u32,0x1b020u32),(0x104,0x1b024),(0x108,0x1b028),(0x10c,0x1b02c)].into_iter() {load(&mut code,offset,4,dest);}
        // Command queue setup (sel=0) + notify.
        for (offset,value) in [(0x30,0),(0x38,8),(0x80,0x20000),(0x90,0x20100),(0xa0,0x20200),(0x44,1),(0x50,0)] {write(&mut code,offset,value);}
        // Interrupt status + ACK + re-read.
        load(&mut code,0x60,4,0x1b014);write(&mut code,0x64,1);load(&mut code,0x60,4,0x1b018);
        // Reset + read status.
        write(&mut code,0x70,0);load(&mut code,0x70,4,0x1b01c);
        code.push(0xf4); // HLT
        let physical=if long {cpu::DATA} else {cpu::ENTRY};f.ram[physical..physical+code.len()].copy_from_slice(&code);
        with_guest_at(&[],0,cpu::RAM_BYTES,|partition,memory| {
            unsafe {std::ptr::copy_nonoverlapping(f.ram.as_ptr(),memory.address.cast::<u8>(),f.ram.len());}
            configure_fixture(partition,&f)?;if long {set_registers(partition,&[0x10],&[Register::scalar(cpu::ALIAS)])?;}
            let emulator=Emulator::new()?;
            let mut ctx=MmioContext{partition:partition.0,ram:memory.address.cast(),bytes:memory.bytes,device:MmioDevice::Gpu(crate::virtio_gpu_mmio::GpuMmio::new()),error:None,translations:0,accesses:0};
            let mut backend=crate::virtio_gpu::ScanoutBackend{surface:crate::virtio_gpu::Surface{width:0,height:0,format:crate::virtio_gpu::FORMAT_XRGB8888,pitch:0,data:Vec::new()}};
            let mut serviced=0;let mut irq_seen=false;
            let (result,timed_out)=bounded(partition,5000,|| {
                for _ in 0..128 {
                    if ctx.device.irq_asserted() {irq_seen=true;}
                    let exit=run_once(partition)?;
                    match exit.reason() {
                        8=>return Ok(Ok(())),
                        1=>{
                            emulator.emulate(&mut ctx,&exit,long)?;
                            if ctx.device.work_pending() {
                                let ram=unsafe {std::slice::from_raw_parts_mut(ctx.ram,ctx.bytes)};
                                let mut memory=crate::guest_memory::GuestMemory::new(vec![crate::guest_memory::RamRegion{gpa:0,bytes:ram,writable:true}])?;
                                if let MmioDevice::Gpu(device)=&mut ctx.device {
                                    device.sync_scanout(&mut backend)?;
                                    let done=device.service(&mut memory,&mut backend,8)?;
                                    serviced+=done.len();
                                } else {return Err("GPU MMIO smoke context is not a gpu device".into());}
                            }
                        },
                        2=>{
                            let io=crate::io::IoExit::decode(&exit.0)?;
                            return Err(format!("Unexpected GPU MMIO fixture port access port=0x{:x}",io.port));
                        },
                        reason=>return Err(format!("Unexpected GPU MMIO fixture exit 0x{reason:x}"))
                    }
                }Ok(Err("GPU MMIO fixture exit budget exceeded".to_string()))
            })?;
            if timed_out {return Err("GPU MMIO fixture deadline exceeded".into());}result?;
            // Verify register reads: device ID, version, vendor, queue num, status after DRIVER_OK.
            let bytes=unsafe {std::slice::from_raw_parts(memory.address.cast::<u8>().add(0x1b000),0x30)};
            for (n,expected) in [0x10u32,2,0x5358454e,256,15].into_iter().enumerate() {
                let actual=u32::from_le_bytes(bytes[n*4..n*4+4].try_into().unwrap());if actual!=expected {return Err(format!("GPU MMIO register {n} mismatch 0x{actual:x}"));}
            }
            // Verify scanout geometry readback (config registers at 0x1b020..0x1b02c).
            for (n,expected) in [8u32,8,3,32].into_iter().enumerate() {
                let actual=u32::from_le_bytes(bytes[(8+n)*4..(8+n)*4+4].try_into().unwrap());if actual!=expected {return Err(format!("GPU scanout geometry register {n} mismatch 0x{actual:x}"));}
            }
            // Verify interrupt was asserted and cleared by ACK.
            let irq_before=u32::from_le_bytes(bytes[5*4..5*4+4].try_into().unwrap());
            let irq_after=u32::from_le_bytes(bytes[6*4..6*4+4].try_into().unwrap());
            if irq_before==0||irq_after!=0 {return Err("GPU MMIO interrupt status/ACK mismatch".into());}
            // Verify status after reset.
            let status_reset=u32::from_le_bytes(bytes[7*4..7*4+4].try_into().unwrap());
            if status_reset!=0 {return Err("GPU MMIO status not cleared by reset".into());}
            // Verify FILL_2D completion: status byte written with 0.
            let fill_status=u8::from_le(unsafe {std::slice::from_raw_parts(ctx.ram.add(0x20400),1)}[0]);
            if fill_status!=0 {return Err("GPU MMIO FILL_2D completion status not success".into());}
            // Verify used-ring index advanced.
            let used_idx=u16::from_le_bytes(unsafe {std::slice::from_raw_parts(ctx.ram.add(0x20202),2)}.try_into().unwrap());
            if used_idx!=1 {return Err("GPU MMIO used-ring index mismatch".into());}
            // Verify real rasterization: the 4x4 red rectangle is in the scanout, rest is clear.
            let surface=backend.scanout();
            if surface.width!=8||surface.height!=8||surface.pitch!=32 {return Err("GPU scanout surface geometry mismatch".into());}
            let red=[0x00u8,0x00,0xff,0x00];
            for row in 0..4u32 {
                let start=(row as usize)*32;
                for col in 0..4u32 {
                    let at=start+(col as usize)*4;
                    if surface.data[at..at+4]!=red {return Err("GPU scanout fill pixel mismatch".into());}
                }
                for col in 4..8u32 {
                    let at=start+(col as usize)*4;
                    if surface.data[at..at+4]!=[0;4] {return Err("GPU scanout clear pixel mismatch".into());}
                }
            }
            for row in 4..8u32 {
                let start=(row as usize)*32;
                if surface.data[start..start+32]!=[0;32] {return Err("GPU scanout clear row mismatch".into());}
            }
            if !irq_seen {return Err("GPU MMIO interrupt was never asserted".into());}
            if serviced!=1 {return Err("GPU MMIO did not complete the command request".into());}
            if ctx.accesses<10||(long&&ctx.translations==0) {return Err("Missing real GPU MMIO/translation callback execution".into());}
            Ok(())
        })?;
    }Ok(())
}

/// Live GPIO + I2C + SPI MMIO smoke: exercises all three platform device
/// controllers through real WHP MMIO emulation. Each device is tested with
/// Live WHP MMIO smoke for the platform GPIO/I2C/SPI controllers. Each device
/// is exercised in its own guest session (one `MmioContext` per device), mirroring
/// the proven block/net/audio smokes: the emulator's `emu_memory` callback routes
/// RAM vs MMIO internally, so every reason-1 exit is handed straight to
/// `emulator.emulate`. Verifies identity-mapped MMIO register write/read round-trips
/// in both long mode and protected mode.
pub fn platform_mmio_smoke()->Result<(),String> {
    use crate::cpu;
    for long in [false,true] {
        let mode=if long {cpu::CpuMode::Long64} else {cpu::CpuMode::Protected32};
        let mut f=cpu::fixture(mode)?;
        // Map the platform MMIO bases in guest page tables (long mode only).
        // MMIO_BASE (0xd0000000, 2MB-aligned) as GPA makes VA 0xd0003000->GPA 0xd0003000,
        // VA 0xd0004000->GPA 0xd0004000, VA 0xd0005000->GPA 0xd0005000 (identity).
        if long {
            f.ram[0x2018..0x2020].copy_from_slice(&0x7003u64.to_le_bytes());
            f.ram[0x7400..0x7408].copy_from_slice(&(MMIO_BASE|0x83).to_le_bytes());
        }
        // GPIO session: direction/output/status writes + readbacks.
        platform_device_session(long,&f,MMIO_GPIO_BASE,
            &[(0x00,0xffffffff),(0x04,0xdeadbeef),(0x0c,0x42)],
            &[(0x00,0x1b000),(0x04,0x1b004),(0x08,0x1b008),(0x0c,0x1b00c)],
            MmioDevice::Gpio(crate::gpio::GpioController::new()),
            &|b: &[u8]| {
                let gpio_dir=u32::from_le_bytes(b[0x00..0x04].try_into().unwrap());
                let gpio_out=u32::from_le_bytes(b[0x04..0x08].try_into().unwrap());
                let gpio_inp=u32::from_le_bytes(b[0x08..0x0c].try_into().unwrap());
                let gpio_sts=u32::from_le_bytes(b[0x0c..0x10].try_into().unwrap());
                if gpio_dir!=0xffffffff {return Err(format!("GPIO direction mismatch 0x{gpio_dir:x}"));}
                if gpio_out!=0xdeadbeef {return Err(format!("GPIO output mismatch 0x{gpio_out:x}"));}
                if gpio_inp!=0 {return Err(format!("GPIO input mismatch 0x{gpio_inp:x}"));}
                if gpio_sts!=0x42 {return Err(format!("GPIO status mismatch 0x{gpio_sts:x}"));}
                Ok(())
            })?;
        // I2C session: control/address/data writes + readbacks.
        platform_device_session(long,&f,MMIO_I2C_BASE,
            &[(0x00,0x01),(0x08,0x50),(0x0c,0xab)],
            &[(0x04,0x1b010),(0x08,0x1b014),(0x0c,0x1b018)],
            MmioDevice::I2c(crate::i2c::I2cController::new()),
            &|b: &[u8]| {
                let i2c_sts=u32::from_le_bytes(b[0x10..0x14].try_into().unwrap());
                let i2c_addr=u32::from_le_bytes(b[0x14..0x18].try_into().unwrap());
                let i2c_data=u32::from_le_bytes(b[0x18..0x1c].try_into().unwrap());
                if i2c_sts!=0 {return Err(format!("I2C status mismatch 0x{i2c_sts:x}"));}
                if i2c_addr!=0x50 {return Err(format!("I2C address mismatch 0x{i2c_addr:x}"));}
                if i2c_data!=0xab {return Err(format!("I2C data mismatch 0x{i2c_data:x}"));}
                Ok(())
            })?;
        // SPI session: control/clock_divider/tx writes + readbacks.
        platform_device_session(long,&f,MMIO_SPI_BASE,
            &[(0x00,0x01),(0x08,0x10),(0x0c,0xa5)],
            &[(0x04,0x1b020),(0x10,0x1b024),(0x08,0x1b028)],
            MmioDevice::Spi(crate::spi::SpiController::new()),
            &|b: &[u8]| {
                let spi_sts=u32::from_le_bytes(b[0x20..0x24].try_into().unwrap());
                let spi_rx=u32::from_le_bytes(b[0x24..0x28].try_into().unwrap());
                let spi_clk=u32::from_le_bytes(b[0x28..0x2c].try_into().unwrap());
                if spi_sts!=1 {return Err(format!("SPI status mismatch 0x{spi_sts:x}"));}
                if spi_rx!=0xa5 {return Err(format!("SPI rx mismatch 0x{spi_rx:x}"));}
                if spi_clk!=0x10 {return Err(format!("SPI clock divider mismatch 0x{spi_clk:x}"));}
                Ok(())
            })?;
    }Ok(())
}

/// Run one platform-device MMIO session: build raw x86 code that writes the given
/// registers and reads them back into guest RAM, place it in the fixture, then drive
/// the vCPU through the real WHP emulator. Every reason-1 exit is emulated (the
/// `emu_memory` callback routes RAM vs this device's MMIO page internally); HLT ends
/// the session. `verify` checks the readback bytes at guest offset 0x1b000.
fn platform_device_session(
    long: bool,
    f: &crate::cpu::Fixture,
    base: u64,
    writes: &[(u32, u32)],
    loads: &[(u32, u32)],
    device: MmioDevice,
    verify: &dyn Fn(&[u8]) -> Result<(), String>,
) -> Result<(), String> {
    use crate::cpu;
    let mut code=Vec::new();
    let load=|code:&mut Vec<u8>,base:u64,offset:u32,dest:u32| {
        if long {code.push(0x67);}code.push(0xa1);code.extend_from_slice(&((base as u32)+offset).to_le_bytes());
        if long {code.push(0x67);}code.push(0xa3);code.extend_from_slice(&dest.to_le_bytes());
    };
    let write=|code:&mut Vec<u8>,base:u64,offset:u32,value:u32| {
        code.push(0xb8);code.extend_from_slice(&value.to_le_bytes());
        if long {code.push(0x67);}code.push(0xa3);code.extend_from_slice(&((base as u32)+offset).to_le_bytes());
    };
    for &(off,val) in writes {write(&mut code,base,off,val);}
    for &(off,dest) in loads {load(&mut code,base,off,dest);}
    code.push(0xf4); // HLT
    let physical=if long {cpu::DATA} else {cpu::ENTRY};
    with_guest_at(&[],0,cpu::RAM_BYTES,|partition,memory| {
        unsafe {
            std::ptr::copy_nonoverlapping(f.ram.as_ptr(),memory.address.cast::<u8>(),f.ram.len());
            std::ptr::copy_nonoverlapping(code.as_ptr(),memory.address.cast::<u8>().add(physical),code.len());
        }
        configure_fixture(partition,f)?;
        if long {set_registers(partition,&[0x10],&[Register::scalar(cpu::ALIAS)])?;}
        let emulator=Emulator::new()?;
        let mut ctx=MmioContext{partition:partition.0,ram:memory.address.cast(),bytes:memory.bytes,device,error:None,translations:0,accesses:0};
        let (result,timed_out)=bounded(partition,5000,|| {
            for _ in 0..200 {
                let exit=run_once(partition)?;
                match exit.reason() {
                    8=>return Ok(Ok(())),
                    1=>{emulator.emulate(&mut ctx,&exit,long)?;},
                    2=>return Err("Unexpected platform device session port exit".to_string()),
                    reason=>return Err(format!("Unexpected platform device session exit 0x{reason:x}"))
                }
            }Ok(Err("Platform device session exit budget exceeded".to_string()))
        })?;
        if timed_out {return Err("Platform device session deadline exceeded".into());}result?;
        let bytes=unsafe {std::slice::from_raw_parts(memory.address.cast::<u8>().add(0x1b000),0x30)};
        verify(bytes)?;
        if ctx.accesses<6||(long&&ctx.translations==0) {return Err(format!("Missing real platform device MMIO callback execution: {} accesses",ctx.accesses));}
        Ok(())
    })
}
