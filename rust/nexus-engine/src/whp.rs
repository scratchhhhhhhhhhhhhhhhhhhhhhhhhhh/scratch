//! Direct Windows Hypervisor Platform API — no QEMU dependency.
//! Bounded real-mode CPU/RAM, private port I/O and cancellation tests. No Android machine.

/// Host OS identity captured by a physical-run check. `RtlGetVersion` always
/// returns the real installed version (it is not affected by the executable's
/// compatibility manifest, unlike `GetVersionEx`).
#[derive(Clone,Copy,Debug)]
pub struct HostOs {pub major:u32,pub minor:u32,pub build:u32,pub platform:u32,pub product_type:u8}
impl HostOs {
    /// True for a Windows 10/11 client workstation (product type 1 = VER_NT_WORKSTATION).
    /// Windows 11 still reports major version 10, so this covers both.
    pub fn is_windows_client(&self)->bool {self.major==10&&self.product_type==1}
    pub fn display(&self)->String {format!("Windows {}.{} build {} (product type {})",self.major,self.minor,self.build,self.product_type)}
}

#[cfg(windows)]
mod windows {
    use std::ffi::c_void;
    include!("whp_mmio.rs");
    include!("whp_smp.rs");
    // Field names mirror the Windows OSVERSIONINFOEXW ABI exactly (repr(C) FFI),
    // so they must not be renamed to snake_case.
    #[repr(C)] #[allow(non_snake_case)] struct OsVersionInfoExW {dwOSVersionInfoSize:u32,dwMajorVersion:u32,dwMinorVersion:u32,dwBuildNumber:u32,dwPlatformId:u32,szCSDVersion:[u16;128],wServicePackMajor:u16,wServicePackMinor:u16,wSuiteMask:u16,wProductType:u8,wReserved:u8}
    #[link(name="ntdll")] extern "system" {fn RtlGetVersion(info:*mut OsVersionInfoExW)->i32;}
    /// Query the real host OS version/build/product type via `RtlGetVersion`.
    pub fn host_os()->Result<crate::whp::HostOs,String> {
        let mut info=OsVersionInfoExW{dwOSVersionInfoSize:std::mem::size_of::<OsVersionInfoExW>() as u32,dwMajorVersion:0,dwMinorVersion:0,dwBuildNumber:0,dwPlatformId:0,szCSDVersion:[0;128],wServicePackMajor:0,wServicePackMinor:0,wSuiteMask:0,wProductType:0,wReserved:0};
        unsafe { if RtlGetVersion(&mut info)!=0 {return Err("RtlGetVersion failed".into());} }
        Ok(crate::whp::HostOs{major:info.dwMajorVersion,minor:info.dwMinorVersion,build:info.dwBuildNumber,platform:info.dwPlatformId,product_type:info.wProductType})
    }
    #[link(name = "WinHvPlatform")]
    extern "system" {
        fn WHvGetCapability(code:u32, buffer:*mut c_void, size:u32, written:*mut u32)->i32;
        fn WHvCreatePartition(partition:*mut *mut c_void)->i32;
        fn WHvSetPartitionProperty(partition:*mut c_void, code:u32, property:*const c_void, size:u32)->i32;
        fn WHvSetupPartition(partition:*mut c_void)->i32;
        fn WHvDeletePartition(partition:*mut c_void)->i32;
        fn WHvMapGpaRange(partition:*mut c_void, source:*mut c_void, guest:u64, size:u64, flags:u32)->i32;
        fn WHvUnmapGpaRange(partition:*mut c_void, guest:u64, size:u64)->i32;
        fn WHvCreateVirtualProcessor(partition:*mut c_void,index:u32,flags:u32)->i32;
        fn WHvDeleteVirtualProcessor(partition:*mut c_void,index:u32)->i32;
        fn WHvSetVirtualProcessorRegisters(partition:*mut c_void,index:u32,names:*const u32,count:u32,values:*const Register)->i32;
        fn WHvGetVirtualProcessorRegisters(partition:*mut c_void,index:u32,names:*const u32,count:u32,values:*mut Register)->i32;
        fn WHvRunVirtualProcessor(partition:*mut c_void,index:u32,context:*mut c_void,size:u32)->i32;
        fn WHvCancelRunVirtualProcessor(partition:*mut c_void,index:u32,flags:u32)->i32;
    }
    #[link(name="kernel32")]
    extern "system" {
        fn VirtualAlloc(address:*mut c_void,size:usize,kind:u32,protection:u32)->*mut c_void;
        fn VirtualFree(address:*mut c_void,size:usize,kind:u32)->i32;
    }
    // Windows SDK WHV_REGISTER_VALUE and WHV_RUN_VP_EXIT_CONTEXT (AMD64).
    #[repr(C,align(16))]
    #[derive(Clone,Copy)]
    struct Register([u8;16]);
    impl Register {
        fn scalar(value:u64)->Self { let mut r=Self([0;16]);r.0[..8].copy_from_slice(&value.to_le_bytes());r }
        fn segment(attributes:u16)->Self {Self::flat_segment(0,0xffff,attributes)}
        fn flat_segment(selector:u16,limit:u32,attributes:u16)->Self {
            let mut r=Self([0;16]);r.0[8..12].copy_from_slice(&limit.to_le_bytes());
            r.0[12..14].copy_from_slice(&selector.to_le_bytes());r.0[14..16].copy_from_slice(&attributes.to_le_bytes());r
        }
        fn table(base:u64,limit:u16)->Self {
            let mut r=Self([0;16]);r.0[6..8].copy_from_slice(&limit.to_le_bytes());r.0[8..16].copy_from_slice(&base.to_le_bytes());r
        }
    }
    #[repr(C,align(16))]
    struct ExitContext([u8;224]);
    struct Mapping<'a> { partition:&'a Partition, address:*mut c_void, bytes:usize,guest:u64 }
    impl Drop for Mapping<'_> {fn drop(&mut self) {unsafe {
        if WHvUnmapGpaRange(self.partition.0,self.guest,self.bytes as u64)<0 {eprintln!("NATIVE_TEARDOWN_ERROR GPA unmap");}
        if VirtualFree(self.address,0,0x8000)==0 {eprintln!("NATIVE_TEARDOWN_ERROR RAM free");}
    }}}
    struct Processor<'a>(&'a Partition,u32);
    impl Drop for Processor<'_> {fn drop(&mut self) {if unsafe {WHvDeleteVirtualProcessor(self.0.0,self.1)}<0 {eprintln!("NATIVE_TEARDOWN_ERROR vCPU delete {}",self.1);}}}
    fn allocate(partition:&Partition,bytes:usize)->Result<Mapping<'_>,String> {allocate_at(partition,bytes,0)}
    fn allocate_at(partition:&Partition,bytes:usize,guest:u64)->Result<Mapping<'_>,String> {
        if guest%4096!=0||guest.checked_add(bytes as u64).is_none() {return Err("Invalid guest mapping address".into());}
        if bytes==0 || bytes%4096!=0 || bytes>crate::kernel::MAX_RAM {return Err("Invalid guest RAM mapping size".into());}
        let address=unsafe { VirtualAlloc(std::ptr::null_mut(),bytes,0x3000,4) };
        if address.is_null() { return Err("VirtualAlloc failed".into()); }
        let hr=unsafe { WHvMapGpaRange(partition.0,address,guest,bytes as u64,if guest==0 {7} else {3}) };
        if let Err(error)=check(hr,"WHvMapGpaRange") { unsafe { VirtualFree(address,0,0x8000); } return Err(error); }
        Ok(Mapping{partition,address,bytes,guest})

    }
    fn check(hr:i32, operation:&str)->Result<(),String> {
        if hr < 0 { Err(format!("{operation}: HRESULT 0x{:08x}",hr as u32)) } else { Ok(()) }
    }
    struct Partition(*mut c_void,u64,u32,bool);
    impl Drop for Partition {fn drop(&mut self) {if unsafe {WHvDeletePartition(self.0)}<0 {eprintln!("NATIVE_TEARDOWN_ERROR partition delete");}}}
    fn create()->Result<Partition,String> {create_machine(1,false)}
    fn create_machine(processors:u32,apic:bool)->Result<Partition,String> {
        if !(1..=8).contains(&processors)||(!apic&&processors!=1) {return Err("Invalid CPU/APIC configuration".into());}
        let mut present=0u32; let mut written=0u32;
        // WHvCapabilityCodeHypervisorPresent = 0; output is Windows BOOL.
        unsafe { check(WHvGetCapability(0,(&mut present as *mut u32).cast(),4,&mut written),"WHvGetCapability")?; }
        if written!=4 || present==0 { return Err("Windows hypervisor unavailable; this is not an Android boot test".into()); }
        let mut handle=std::ptr::null_mut();
        unsafe { check(WHvCreatePartition(&mut handle),"WHvCreatePartition")?; }
        let mut partition=Partition(handle,0,processors,apic);
        // WHvPartitionPropertyCodeProcessorCount = 0x1fff (Windows SDK).
        unsafe {
            check(WHvSetPartitionProperty(partition.0,0x1fff,(&processors as *const u32).cast(),4),"WHvSetPartitionProperty")?;
            // Native PIT calibration can be too slow/noisy across virtual exits.
            // Publish the actual WHP TSC frequency, never nominal host CPU MHz.
            let mut hz=0u64;let mut clock_written=0u32;
            check(WHvGetCapability(0x1004,(&mut hz as *mut u64).cast(),8,&mut clock_written),"WHP processor clock frequency")?;
            if clock_written!=8 {return Err("WHP processor clock capability size mismatch".into());}
            partition.1=hz;
            let native=std::arch::x86_64::__cpuid(0);
            let clocks=crate::tsc::frequency_leaves(hz,[native.eax,native.ebx,native.ecx,native.edx])?;
            check(WHvSetPartitionProperty(partition.0,0x1004,clocks.as_ptr().cast(),(clocks.len()*32) as u32),"WHP TSC CPUID frequency contract")?;
            let cpuid_exits=if apic {3u64} else {1u64};
            check(WHvSetPartitionProperty(partition.0,1,(&cpuid_exits as *const u64).cast(),8),"WHP CPUID exits")?;
            let leaves=crate::topology::EXIT_LEAVES;
            check(WHvSetPartitionProperty(partition.0,0x1003,leaves.as_ptr().cast(),(leaves.len()*4) as u32),"WHP topology exit leaves")?;
            // APIC MMIO is owned by our explicit frontend, not WHP's opaque
            // built-in controller. Windows 10/Server 2022 cannot trap its SIPIs.
            check(WHvSetupPartition(partition.0),"WHvSetupPartition")?;
        }
        Ok(partition)
    }
    pub fn probe()->Result<(),String> { let _partition=create()?; Ok(()) }
    fn with_guest<T>(code:&[u8], action:impl FnOnce(&Partition,&Mapping<'_>)->Result<T,String>)->Result<T,String> {
        with_guest_at(code,0,4096,action)
    }
    fn with_guest_at<T>(code:&[u8],entry:usize,ram_bytes:usize,action:impl FnOnce(&Partition,&Mapping<'_>)->Result<T,String>)->Result<T,String> {
        with_guest_profile(code,entry,ram_bytes,(1,false,false),action)
    }
    fn with_guest_profile<T>(code:&[u8],entry:usize,ram_bytes:usize,profile:(u32,bool,bool),action:impl FnOnce(&Partition,&Mapping<'_>)->Result<T,String>)->Result<T,String> {
        if entry>0xffff || entry.checked_add(code.len()).filter(|&n|n<=ram_bytes).is_none() {return Err("Test code exceeds real-mode/RAM bounds".into());}
        // Declared before VM resources: the evidence drops after all of them.
        struct TeardownEvidence(bool);
        impl Drop for TeardownEvidence {fn drop(&mut self) {if self.0 {eprintln!("NATIVE_VM_RESOURCES_DROPPED");}}}
        let _evidence=TeardownEvidence(profile.2);
        let partition=create_machine(profile.0,profile.1)?;
        let memory=allocate(&partition,ram_bytes)?;
        unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(),memory.address.cast::<u8>().add(entry),code.len()); }
        let mut processors=Vec::new();
        for index in 0..profile.0 {unsafe {check(WHvCreateVirtualProcessor(partition.0,index,0),"WHvCreateVirtualProcessor")?;}processors.push(Processor(&partition,index));}
        let names=[0x1c,0x1e,0x1f,0x13,0x15,0x12,0x14,0x10,0x11,0x00];
        let values=[Register::scalar(0x60000010),Register::scalar(0),Register::scalar(0),Register::segment(0x9b),Register::segment(0x93),Register::segment(0x93),Register::segment(0x93),Register::scalar(entry as u64),Register::scalar(2),Register::scalar(0)];
        set_registers(&partition,&names,&values)?;
        action(&partition,&memory)
    }
    fn set_registers(partition:&Partition,names:&[u32],values:&[Register])->Result<(),String> {
        if names.len()!=values.len() { return Err("Mismatched register arrays".into()); }
        unsafe { check(WHvSetVirtualProcessorRegisters(partition.0,0,names.as_ptr(),names.len() as u32,values.as_ptr()),"WHvSetVirtualProcessorRegisters") }
    }
    fn accumulator(partition:&Partition)->Result<u64,String> {
        scalar_register(partition,0)
    }
    fn scalar_register(partition:&Partition,name:u32)->Result<u64,String> {
        let mut result=Register([0;16]);
        unsafe {check(WHvGetVirtualProcessorRegisters(partition.0,0,&name,1,&mut result),"WHvGetVirtualProcessorRegisters")?;}
        Ok(u64::from_le_bytes(result.0[..8].try_into().unwrap()))
    }
    fn handle_cpuid(partition:*mut c_void,index:u32,count:u32,apic:bool,context:&ExitContext)->Result<bool,String> {
        if context.reason()!=0x1001 {return Ok(false);}
        let value=|at|u64::from_le_bytes(context.0[at..at+8].try_into().unwrap()) as u32;
        let d=crate::topology::result(value(48),value(56),index,count,apic,[value(80),value(104),value(88),value(96)])?;
        let mode=crate::cpu::CpuMode::from_exit(&context.0)?;
        let rip=u64::from_le_bytes(context.0[32..40].try_into().unwrap());
        let names=[0,3,1,2,0x10];
        let values=[Register::scalar(u64::from(d[0])),Register::scalar(u64::from(d[1])),Register::scalar(u64::from(d[2])),Register::scalar(u64::from(d[3])),Register::scalar(mode.next_rip(rip,context.0[10]&15)?)];
        unsafe {check(WHvSetVirtualProcessorRegisters(partition,index,names.as_ptr(),5,values.as_ptr()),"WHP topology CPUID result")?;}
        Ok(true)
    }
    fn run_once(partition:&Partition)->Result<ExitContext,String> {
        for _ in 0..256 {
            let mut context=ExitContext([0;224]);
            unsafe { check(WHvRunVirtualProcessor(partition.0,0,context.0.as_mut_ptr().cast(),224),"WHvRunVirtualProcessor")?; }
            if !handle_cpuid(partition.0,0,partition.2,partition.3,&context)? {return Ok(context);}
        }
        Err("Consecutive CPUID exit budget exceeded".into())
    }
    impl ExitContext {
        fn reason(&self)->u32 {u32::from_le_bytes(self.0[..4].try_into().unwrap())}
    }
    // Covers the ENTIRE exit loop, not a fresh deadline after each I/O exit.
    // Partition stays alive until action completes AND watchdog joins. A wedged
    // API has a hard process deadline, never a detached thread using freed RAM.
    fn bounded<T>(partition:&Partition,millis:u64,action:impl FnOnce()->Result<T,String>)->Result<(T,bool),String> {
        use std::sync::{Arc,atomic::{AtomicBool,Ordering},mpsc};
        use std::time::Duration;
        let handle=partition.0 as usize;let processors=partition.2;
        let canceled=Arc::new(AtomicBool::new(false));let flag=Arc::clone(&canceled);
        std::thread::scope(|scope| {
        let (tx,rx)=mpsc::channel();
        let watchdog=scope.spawn(move || {
            if matches!(rx.recv_timeout(Duration::from_millis(millis)),Err(mpsc::RecvTimeoutError::Timeout)) {
                flag.store(true,Ordering::SeqCst);
                for index in 0..processors {unsafe { WHvCancelRunVirtualProcessor(handle as *mut c_void,index,0); }}
                if matches!(rx.recv_timeout(Duration::from_secs(5)),Err(mpsc::RecvTimeoutError::Timeout)) {
                    eprintln!("WHP guest deadline exceeded; terminating test process");
                    std::process::exit(2);
                }
            }
        });
        let result=action();
        let _=tx.send(());watchdog.join().map_err(|_|"WHP watchdog panicked")?;
        Ok((result?,canceled.load(Ordering::SeqCst)))
        })
    }
    fn run_to_halt(partition:&Partition,bus:&mut crate::io::TestBus)->Result<usize,String> {
        run_to_halt_mode(partition,bus,crate::cpu::CpuMode::Real16)
    }
    fn run_to_halt_mode(partition:&Partition,bus:&mut crate::io::TestBus,mode:crate::cpu::CpuMode)->Result<usize,String> {
        let (count,timed_out)=bounded(partition,5000,|| {
            // An I/O-exit storm cannot bypass the wall deadline or exit budget.
            for count in 0..64 {
                let context=run_once(partition)?;
                match context.reason() {
                    8=>return Ok(count),
                    2=>{
                        let io=crate::io::IoExit::decode(&context.0)?;
                        let rip=Register::scalar(mode.next_rip(io.rip,io.instruction_len)?);
                        if let Some(rax)=bus.access(io)? {
                            set_registers(partition,&[0,0x10],&[Register::scalar(rax),rip])?;
                        } else {set_registers(partition,&[0x10],&[rip])?;}
                    }
                    reason=>return Err(format!("Unsupported guest exit 0x{reason:x}")),
                }
            }
            Err("Guest exit budget exceeded".into())
        })?;
        if timed_out {return Err("Guest wall deadline exceeded".into());}
        Ok(count)
    }
    const RAM_PROGRAM:&[u8]=&[0xb8,0x17,0x5a,0xa3,0x00,0x02,0x31,0xc0,0xa1,0x00,0x02,0xf4];
    fn check_ram(partition:&Partition,memory:&Mapping<'_>)->Result<(),String> {
        let stored=unsafe {std::slice::from_raw_parts(memory.address.cast::<u8>().add(0x200),2)};
        if accumulator(partition)?!=0x5a17 || stored!=[0x17,0x5a] {return Err("Guest CPU/RAM result mismatch".into());}
        Ok(())
    }
    pub fn cpu_smoke()->Result<(),String> {
        with_guest(RAM_PROGRAM,|partition,memory| {
            run_to_halt(partition,&mut crate::io::TestBus::default())?;
            check_ram(partition,memory)
        })
    }
    pub fn protection_smoke()->Result<(),String> {
        use crate::cpu;
        let f=cpu::protection_fixture()?;
        with_guest_at(&[],0,cpu::RAM_BYTES,|partition,memory| {
            unsafe {std::ptr::copy_nonoverlapping(f.ram.as_ptr(),memory.address.cast::<u8>(),f.ram.len());}
            configure_fixture(partition,&f)?;
            run_to_halt_mode(partition,&mut crate::io::TestBus::default(),cpu::CpuMode::Long64)?;
            let read=|at:usize|->u64 {
                let bytes=unsafe {std::slice::from_raw_parts(memory.address.cast::<u8>().add(at),8)};
                u64::from_le_bytes(bytes.try_into().unwrap())
            };
            if read(cpu::DATA)!=cpu::VALUE64||read(cpu::DATA+8)!=cpu::ALIAS
                ||read(cpu::DATA+16)!=3||read(cpu::INTERRUPT_COUNT)!=2
                ||accumulator(partition)?!=cpu::VALUE64||scalar_register(partition,4)?!=cpu::STACK {
                return Err("Guest page-fault protection/recovery mismatch".into());
            }
            Ok(())
        })
    }
    pub fn transition_smoke()->Result<(),String> {
        use crate::cpu::{self,CpuMode};
        let ram=cpu::transition_image()?;
        with_guest_at(&[],cpu::ENTRY,cpu::RAM_BYTES,|partition,memory| {
            unsafe {std::ptr::copy_nonoverlapping(ram.as_ptr(),memory.address.cast::<u8>(),ram.len());}
            // No configure_fixture call: CR0/CR3/CR4/EFER and CS remain real mode.
            let mut bus=crate::io::TestBus::default();let mut modes=Vec::new();
            let ((),timed_out)=bounded(partition,5000,|| {
                for _ in 0..16 {
                    let context=run_once(partition)?;
                    let mode=CpuMode::from_exit(&context.0)?;
                    match context.reason() {
                        2=>{
                            let io=crate::io::IoExit::decode(&context.0)?;modes.push(mode);
                            let rip=Register::scalar(mode.next_rip(io.rip,io.instruction_len)?);
                            if let Some(rax)=bus.access(io)? {set_registers(partition,&[0,0x10],&[Register::scalar(rax),rip])?;}
                            else {set_registers(partition,&[0x10],&[rip])?;}
                        }
                        8 if mode==CpuMode::Long64=>return Ok(()),
                        reason=>return Err(format!("Guest mode transition failed at exit 0x{reason:x}, mode {mode:?}")),
                    }
                }
                Err("Guest transition exit budget exceeded".into())
            })?;
            if timed_out {return Err("Guest mode transition timed out".into());}
            let stored=unsafe {std::slice::from_raw_parts(memory.address.cast::<u8>().add(cpu::DATA),8)};
            if bus.console!=b"RPL"||modes!=[CpuMode::Real16,CpuMode::Protected32,CpuMode::Long64]
                ||stored!=cpu::VALUE64.to_le_bytes()||accumulator(partition)?!=cpu::VALUE64
                ||scalar_register(partition,0x1c)?&0x80000001!=0x80000001
                ||scalar_register(partition,0x1e)?!=0x1000
                ||scalar_register(partition,0x1f)?&0x20==0
                ||scalar_register(partition,0x2001)?&0x500!=0x500 {
                return Err("Guest-driven mode transition/paging result mismatch".into());
            }
            Ok(())
        })
    }
    pub fn io_smoke()->Result<(),String> {
        let mut code=vec![0xba,0xe9,0x00]; // DX=private debug port, NOT host hardware.
        for byte in b"NEXUS\n" {code.extend_from_slice(&[0xb0,*byte,0xee]);}
        code.extend_from_slice(&[
            0xba,0x04,0x05,                     // DX=test latch
            0x66,0xb8,0x78,0x56,0x34,0x12,     // EAX=12345678
            0x66,0xef,                          // OUT DX,EAX
            0x66,0xb8,0xdd,0xcc,0xbb,0xaa,     // EAX=AABBCCDD
            0xec,0x66,0xa3,0x04,0x02,          // IN AL; store EAX at 204
            0xed,0x66,0xa3,0x08,0x02,          // IN AX; store EAX at 208
            0x66,0xed,0x66,0xa3,0x00,0x02,     // IN EAX; store EAX at 200
            0xf4]);
        with_guest(&code,|partition,memory| {
            let mut bus=crate::io::TestBus::default();
            let count=run_to_halt(partition,&mut bus)?;
            let stored=unsafe {std::slice::from_raw_parts(memory.address.cast::<u8>().add(0x200),12)};
            if count!=10 || bus.console!=b"NEXUS\n" || accumulator(partition)?!=0x12345678
                || stored!=[0x78,0x56,0x34,0x12,0x78,0xcc,0xbb,0xaa,0x78,0x56,0xbb,0xaa] {
                return Err(format!("Guest port-I/O result mismatch: exits={count}, data={stored:?}"));
            }
            Ok(())
        })?;
        // Fail closed on REP/strings, host-port requests and excessive exits.
        for (code,expected) in [
            (&[0xba,0xe9,0x00,0xb9,0x01,0x00,0xf3,0x6e,0xf4][..],"String/REP"),
            (&[0xba,0xff,0xff,0xec,0xf4][..],"Unsupported test port"),
            (&[0xba,0xe9,0x00,0xb0,0x78,0xee,0xeb,0xfd][..],"exit budget"),
        ] {
            let result=with_guest(code,|partition,_|run_to_halt(partition,&mut crate::io::TestBus::default()));
            match result {Err(error) if error.contains(expected)=>{},other=>return Err(format!("Negative I/O gate failed ({expected}): {other:?}"))}
        }
        Ok(())
    }
    fn configure_fixture(partition:&Partition,f:&crate::cpu::Fixture)->Result<(),String> {
        let code=Register::flat_segment(8,u32::MAX,f.code_attributes);
        let data=Register::flat_segment(16,u32::MAX,0xc093);
        set_registers(partition,&[0x1c,0x1e,0x1f,0x2001,0x13,0x15,0x12,0x14,0x16,0x17,0x1b,0x1a,0x10,4,0x11,0],&[
            Register::scalar(f.cr0),Register::scalar(f.cr3),Register::scalar(f.cr4),Register::scalar(f.efer),
            code,data,data,data,data,data,Register::table(crate::cpu::GDT as u64,23),
            Register::table(crate::cpu::IDT as u64,f.idt_limit),Register::scalar(crate::cpu::ENTRY as u64),
            Register::scalar(crate::cpu::STACK),Register::scalar(2),Register::scalar(0)])
    }
    fn mode_fixture(mode:crate::cpu::CpuMode,missing_alias:bool)->Result<(),String> {
        use crate::cpu;
        let mut f=cpu::fixture(mode)?;
        if missing_alias {f.ram[0x4000..0x4008].fill(0);}
        with_guest_at(&[],0,cpu::RAM_BYTES,|partition,memory| {
            // Entire image and mapping have equal, bounded lengths.
            if f.ram.len()!=memory.bytes {return Err("Startup RAM size mismatch".into());}
            unsafe {std::ptr::copy_nonoverlapping(f.ram.as_ptr(),memory.address.cast::<u8>(),f.ram.len());}
            configure_fixture(partition,&f)?;
            let mut bus=crate::io::TestBus::default();
            let result=run_to_halt_mode(partition,&mut bus,f.mode);
            if missing_alias {
                // No page-fault handler: absent alias must lead to triple fault,
                // never a successful HLT or an accidental physical write.
                if !matches!(&result,Err(error) if error=="Unsupported guest exit 0x4") {return Err(format!("Absent PTE did not fail closed: {result:?}"));}
                let stored=unsafe {std::slice::from_raw_parts(memory.address.cast::<u8>().add(cpu::DATA),8)};
                if stored!=[0;8] {return Err("Absent PTE wrote guest data".into());}
                return Ok(());
            }
            if result?!=1 {return Err("Expected exactly one startup debug I/O exit".into());}
            let long=mode==cpu::CpuMode::Long64;
            let expected=if long {cpu::VALUE64} else {cpu::VALUE32};
            let stored=unsafe {std::slice::from_raw_parts(memory.address.cast::<u8>().add(cpu::DATA),8)};
            if accumulator(partition)?!=expected || u64::from_le_bytes(stored.try_into().unwrap())!=expected
                || bus.console!=[if long {b'L'} else {b'P'}] {return Err("Protected/long-mode CPU/RAM result mismatch".into());}
            for (name,value) in [(0x1c,f.cr0),(0x1e,f.cr3),(0x1f,f.cr4),(0x2001,f.efer),(4,cpu::STACK)] {
                if scalar_register(partition,name)?!=value {return Err(format!("Startup state mismatch for register {name:x}"));}
            }
            let count=unsafe {std::slice::from_raw_parts(memory.address.cast::<u8>().add(cpu::INTERRUPT_COUNT),8)};
            if u64::from_le_bytes(count.try_into().unwrap())!=u64::from(long) {return Err("Guest interrupt/IRETQ result mismatch".into());}
            Ok(())
        })
    }
    pub fn protected_smoke()->Result<(),String> {mode_fixture(crate::cpu::CpuMode::Protected32,false)}
    pub fn long_smoke()->Result<(),String> {
        mode_fixture(crate::cpu::CpuMode::Long64,false)?;
        mode_fixture(crate::cpu::CpuMode::Long64,true)
    }
    fn load_kernel_plan<T>(plan:&crate::kernel::KernelPlan,action:impl FnOnce(&Partition,&Mapping<'_>,Option<&Mapping<'_>>)->Result<T,String>)->Result<T,String> {
        use crate::kernel;
        plan.validate_regions()?;
        // Claim the capture exclusively before starting a VM or allowing disk writes.
        let mut output=plan.framebuffer_output.as_ref().map(|path|std::fs::OpenOptions::new().create_new(true).write(true).open(path).map_err(|e|e.to_string())).transpose()?;
        with_guest_profile(&[],0,plan.ram_bytes,(plan.cpu_count,plan.apic_probe,plan.dma_failure.is_some()),|partition,memory| {
            for region in &plan.regions {
                unsafe {std::ptr::copy_nonoverlapping(region.bytes.as_ptr(),memory.address.cast::<u8>().add(region.address),region.bytes.len());}
            }
            if plan.apic_probe {
                let tables=crate::firmware::mp_tables(plan.cpu_count)?;
                unsafe {std::ptr::copy_nonoverlapping(tables.as_ptr(),memory.address.cast::<u8>().add(crate::firmware::MP_BASE),tables.len());}
            }
            let clock_command=plan.command_with_tsc_hz(partition.1)?;
            unsafe {std::ptr::copy_nonoverlapping(clock_command.as_ptr(),memory.address.cast::<u8>().add(kernel::CMDLINE),clock_command.len());}
            let code=Register::flat_segment(0x10,u32::MAX,0xc09b);
            let data=Register::flat_segment(0x18,u32::MAX,0xc093);
            set_registers(partition,&[0x1c,0x1e,0x1f,0x2001,0x13,0x15,0x12,0x14,0x16,0x17,0x1b,0x1a,0x10,4,0x11,0,1,2,3,5,6,7],&[
                Register::scalar(0x11),Register::scalar(0),Register::scalar(0),Register::scalar(0),code,data,data,data,data,data,
                Register::table(kernel::GDT as u64,31),Register::table(0,0),Register::scalar(plan.entry),Register::scalar(kernel::STACK),Register::scalar(2),
                Register::scalar(0),Register::scalar(0),Register::scalar(0),Register::scalar(0),Register::scalar(0),Register::scalar(kernel::BOOT_PARAMS as u64),Register::scalar(0)])?;
            let surface=if plan.framebuffer_output.is_some() {Some(allocate_at(partition,crate::framebuffer::MAPPED_BYTES,crate::framebuffer::GPA)?)} else {None};
            let result=action(partition,memory,surface.as_ref());
            if let (Some(session),Some(surface))=(plan.player.as_ref(),surface.as_ref()) {
                // Publish the final stopped surface and give the presenter a bounded drain.
                let pixels=unsafe {std::slice::from_raw_parts(surface.address.cast::<u8>(),surface.bytes)};
                session.publish(pixels)?;
                let expected=u64::from(!crate::image::crc32(&pixels[..crate::framebuffer::PIXEL_BYTES],!0));
                let until=std::time::Instant::now()+std::time::Duration::from_millis(500);
                while !session.stopped()&&session.presented_crc32.load(std::sync::atomic::Ordering::Acquire)!=expected&&std::time::Instant::now()<until {std::thread::sleep(std::time::Duration::from_millis(5));}
            }
            if let (Some(file),Some(surface))=(&mut output,&surface) {
                // Run returned; no vCPU or worker is accessing the framebuffer during capture.
                let pixels=unsafe {std::slice::from_raw_parts(surface.address.cast::<u8>(),surface.bytes)};
                let capture=crate::framebuffer::write_capture(file,pixels);
                if result.is_ok() {capture?;}
            }
            result
        })
    }
    pub fn kernel_contract_smoke()->Result<(),String> {
        use crate::kernel;
        let plan=kernel::KernelPlan::build(&kernel::contract_image(),Some(b"INIT"),"nexus",kernel::DEFAULT_RAM)?;
        load_kernel_plan(&plan,|partition,_,_| {
            run_to_halt_mode(partition,&mut crate::io::TestBus::default(),crate::cpu::CpuMode::Protected32)?;
            for (name,value) in [(0,kernel::CMDLINE as u64),(3,u32::from_le_bytes(*b"nexu") as u64),
                (1,plan.entry),(2,u32::from_le_bytes(*b"INIT") as u64),(7,plan.initrd_address.unwrap() as u64),(5,3),(6,kernel::BOOT_PARAMS as u64)] {
                if scalar_register(partition,name)?!=value {return Err(format!("Kernel handshake register {name:x} mismatch"));}
            }
            Ok(())
        })
    }
    // Cancellation is only a scheduling rendezvous; it never fabricates a guest tick.
    // Scoped workers join before partition teardown on success, error AND panic.
    // Channels live inside the scope so unwinding disconnects them before join.
    #[link(name="winmm")]
    extern "system" {fn timeBeginPeriod(period:u32)->u32;fn timeEndPeriod(period:u32)->u32;}
    struct TimerResolution;
    impl TimerResolution {fn acquire()->Result<Self,String> {
        if unsafe {timeBeginPeriod(1)}!=0 {return Err("Cannot request 1ms Windows scheduling resolution".into());}Ok(Self)
    }}
    impl Drop for TimerResolution {fn drop(&mut self) {unsafe {timeEndPeriod(1);}}}
    fn timer_pump<T>(partition:&Partition,action:impl FnOnce()->Result<T,String>)->Result<T,String> {
        use std::sync::mpsc;
        use std::time::Duration;
        // Default Windows timer granularity (~15.6ms) can miss Linux's real
        // PIT/IOAPIC boot check despite a requested 1ms rendezvous. Scope the
        // resolution request to VM lifetime and restore it on errors/panics too.
        let _resolution=TimerResolution::acquire()?;
        let handle=partition.0 as usize;let processors=partition.2;
        std::thread::scope(|scope| {
        let (tx,rx)=mpsc::channel();
        let worker=scope.spawn(move || {
            while matches!(rx.recv_timeout(Duration::from_millis(1)),Err(mpsc::RecvTimeoutError::Timeout)) {
                for index in 0..processors {unsafe {WHvCancelRunVirtualProcessor(handle as *mut c_void,index,0);}}
            }
        });
        let result=action();let _=tx.send(());worker.join().map_err(|_|"Timer rendezvous thread panicked")?;result
        })
    }
    fn queue_interrupt(partition:&Partition,platform:&mut crate::platform::EarlyPlatform)->Result<Option<u8>,String> {
        let Some(vector)=platform.pic.pending_vector() else {return Ok(None);};
        // Only inject in an open interrupt window. IF=0 or STI/MOV-SS shadow
        // would make external injection invalid on some WHP versions.
        if scalar_register(partition,0x11)?&0x200==0||scalar_register(partition,0x80000001)?&1!=0 {return Ok(None);}
        // Never overwrite an event already queued in WHP.
        if scalar_register(partition,0x80000000)?&1!=0 {return Ok(None);}
        set_registers(partition,&[0x80000000],&[Register::scalar(1|(u64::from(vector)<<16))])?;
        platform.pic.acknowledge()?;Ok(Some(vector))
    }
    #[derive(Clone,Copy,PartialEq)]
    enum RunGoal {Diagnostic,TimerFixture,AndroidInit,AndroidObserve,NativeBridge,NativeApk,AndroidHome}
    #[allow(clippy::too_many_arguments)]
    fn run_platform(partition:&Partition,platform:&mut crate::platform::EarlyPlatform,
        serial:&mut dyn std::io::Write,trace:&mut crate::timing::BootTrace,start:std::time::Instant,
        run:(RunGoal,Option<(&Mapping<'_>,&crate::player::Session)>),mut disk:Option<&mut LiveBlock>,mut net:Option<&mut LiveNet>,mut audio:Option<&mut LiveAudio>,mut gpu:Option<&mut LiveGpu>,mut null:Option<&mut LiveNull>)->Result<(),String> {
        let (goal,display)=run;let mut last_frame=std::time::Instant::now();
        let fixture=goal==RunGoal::TimerFixture;
        // Diagnostic wall budgets. Android/APK runs need room for CI scheduling variance;
        // they still terminate via this global deadline and the bridge's completion exit.
        let limit=if goal==RunGoal::AndroidHome {460000} else if goal==RunGoal::NativeApk {420000} else if matches!(goal,RunGoal::AndroidInit|RunGoal::AndroidObserve|RunGoal::NativeBridge) {180000} else {30000};
        let elapsed_ms=||start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        let mut halted=false;let mut runs=0;let mut last=(0u32,0u64,0u16);
        timer_pump(partition,|| {
            let (result,timed_out)=bounded(partition,limit,|| {
                loop {
                    if let Some((surface,session))=display {
                        if session.stopped() {return Err("Player window closed; guest stopped safely (not Android shutdown)".into());}
                        if last_frame.elapsed()>=std::time::Duration::from_millis(33) {
                            // vCPU is stopped here. No worker accesses guest RAM.
                            let pixels=unsafe {std::slice::from_raw_parts(surface.address.cast::<u8>(),surface.bytes)};
                            session.publish(pixels)?;last_frame=std::time::Instant::now();
                        }
                    }
                    if elapsed_ms()>=limit {return Err("Platform wall deadline exceeded".into());}
                    let now_ns=start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
                    platform.tick(now_ns);
                    if let Some(bridge)=&mut platform.bridge {
                        bridge.tick()?;
                        // Drain player UI commands into the bridge's outbound queue.
                        if let Some((_,session))=display {
                            while let Some(cmd)=session.next_command() {
                                bridge.client.push_command(&cmd)?;
                            }
                            if let Some(app)=bridge.client.app() {let _=session.set_app(app);}
                        }
                        if matches!(goal,RunGoal::NativeBridge|RunGoal::NativeApk|RunGoal::AndroidHome)&&bridge.client.complete() {
                            if bridge.client.installing() {
                                println!("NATIVE_BRIDGE_APK_INSTALL_VERIFY_UNINSTALL_OK");
                                // Home diagnostic completed at bridge VERIFY_TEST_APK OK.
                                if trace.android_home_ready_ms.is_none() {
                                    trace.android_home_ready_ms=Some(elapsed_ms());
                                }
                            } else if bridge.client.home_checking() {
                                // Clean-launch Home certification: CHECK_HOME OK over
                                // COM2 with no APK install. Records launch-to-Home.
                                println!("NATIVE_BRIDGE_HOME_VERIFIED ms={}",elapsed_ms());
                                if trace.android_home_ready_ms.is_none() {
                                    trace.android_home_ready_ms=Some(elapsed_ms());
                                }
                            } else {println!("NATIVE_BRIDGE_QUERY_OK app={} transport=private-COM2; Android actions/install/network/audio not verified",bridge.client.app().unwrap_or("-"));}
                            return Ok(Ok(()));
                        }
                    }
                    if let Some((_,session))=display {
                        if platform.uart.can_receive() {if let Some(byte)=session.next_byte() {platform.uart.receive(byte)?;}}
                    }
                    if let Some(disk)=disk.as_ref() {platform.pic.set_irq5(disk.context.device.irq_asserted());}
                    if let Some(net)=net.as_ref() {platform.pic.set_irq6(net.context.device.irq_asserted());}
                    if let Some(audio)=audio.as_ref() {platform.pic.set_irq7(audio.context.device.irq_asserted());}
                    if let Some(gpu)=gpu.as_ref() {platform.pic.set_irq8(gpu.context.device.irq_asserted());}
                    if let Some(vector)=queue_interrupt(partition,platform)? {match vector&7 {0=>trace.irq0_injections+=1,3=>trace.irq3_injections+=1,4=>trace.irq4_injections+=1,_=>trace.irq5_injections+=1}halted=false;}
                    trace.pit0_programs=platform.pit0.programs;trace.pit0_edges=platform.pit0.edges;
                    if halted {std::thread::sleep(std::time::Duration::from_millis(1));continue;}
                    if runs>=if goal==RunGoal::NativeApk||goal==RunGoal::AndroidHome {2_000_000} else if goal==RunGoal::NativeBridge {1_000_000} else {250000} {break;}
                    if trace.vcpu_run_requested_ms.is_none() {trace.vcpu_run_requested_ms=Some(elapsed_ms());}
                    let context=run_once(partition)?;trace.exits+=1;runs+=1;
                    last.0=context.reason();last.1=u64::from_le_bytes(context.0[32..40].try_into().unwrap());
                    match context.reason() {
                        1=>{
                            // Route the MMIO exit to the device whose base owns the GPA.
                            let gpa=u64::from_le_bytes(context.0[48..56].try_into().unwrap());
                            if let Some(audio)=audio.as_mut() {
                                if (MMIO_AUDIO_BASE..MMIO_AUDIO_BASE+4096).contains(&gpa) {audio.access(&context)?;continue;}
                            }
                            if let Some(net)=net.as_mut() {
                                if (MMIO_NET_BASE..MMIO_NET_BASE+4096).contains(&gpa) {net.access(&context)?;continue;}
                            }
                            if let Some(gpu)=gpu.as_mut() {
                                if (MMIO_GPU_BASE..MMIO_GPU_BASE+4096).contains(&gpa) {gpu.access(&context)?;continue;}
                            }
                            if let Some(disk)=disk.as_mut() {
                                if (MMIO_BASE..MMIO_BASE+4096).contains(&gpa) {disk.access(&context)?;continue;}
                            }
                            // Unmapped MMIO or framebuffer slot: route to the null device so the
                            // kernel's virtio-mmio probe and fbdev access see the "no device"
                            // magic and skip these slots gracefully.
                            if (MMIO_BASE..MMIO_BASE+8*4096).contains(&gpa) || (crate::framebuffer::GPA..crate::framebuffer::GPA+crate::framebuffer::MAPPED_BYTES as u64).contains(&gpa) {
                                null.as_mut().ok_or("Unmapped MMIO/framebuffer slot without a null device")?.access(&context)?;
                                continue;
                            }
                            return Err(format!("Emulator memory access outside RAM/device page at gpa=0x{gpa:x}"));
                        }
                        0x2001=>continue, // Scheduling rendezvous; wall deadline remains global.
                        8=>{
                            if fixture&&trace.irq0_injections>0 {return Ok(Ok(()));}
                            let flags=scalar_register(partition,0x11)?;
                            if flags&0x200==0 {return Err("Guest halted with interrupts disabled; no completed boot".into());}
                            let rip=u64::from_le_bytes(context.0[32..40].try_into().unwrap());
                            // WHP reports HLT after execution (length=0, RIP already advanced).
                            // Do not skip the instruction after HLT.
                            if fixture&&rip!=(crate::cpu::ENTRY+3) as u64 {return Err(format!("Unexpected HLT RIP 0x{rip:x}, length={}",context.0[10]&15));}
                            trace.halt_waits+=1;halted=true;
                        }
                        2=>{
                            let io=crate::io::IoExit::decode(&context.0)?;last.2=io.port;
                            let mode=crate::cpu::CpuMode::from_exit(&context.0)?;
                            let rip=Register::scalar(mode.next_rip(io.rip,io.instruction_len)?);
                            let before=platform.uart.output.len();
                            let io_ns=start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
                            let response=platform.access(io,io_ns);
                            trace.pit_programs=platform.pit.programs;trace.pit_accesses=platform.pit.accesses;
                            trace.pic_accesses=platform.pic.accesses;trace.pic_initializations=platform.pic.initializations;
                            let input=response.map_err(|e|format!("{e}; mode={mode:?}, RIP=0x{:x}",io.rip))?;
                            let emitted=&platform.uart.output[before..];
                            if !emitted.is_empty() {
                                serial.write_all(emitted).map_err(|e|e.to_string())?;
                                trace.observe_serial(emitted,elapsed_ms());
                                if emitted.contains(&b'\n') {serial.flush().map_err(|e|e.to_string())?;}
                            }
                            if let Some(rax)=input {set_registers(partition,&[0,0x10],&[Register::scalar(rax),rip])?;}
                            else {set_registers(partition,&[0x10],&[rip])?;}
                            if (goal==RunGoal::Diagnostic&&trace.linux_userspace_ready_ms.is_some())||(goal==RunGoal::AndroidInit&&trace.android_init_second_stage_ms.is_some()) {return Ok(Ok(()));}
                        }
                        reason=>{
                            let rip=u64::from_le_bytes(context.0[32..40].try_into().unwrap());
                            return Ok(Err(format!("Kernel stopped at unsupported exit 0x{reason:x}, RIP=0x{rip:x}; full platform boot is not implemented")));
                        }
                    }
                }
                Ok(Err(format!("Kernel exit budget exceeded; last reason=0x{:x}, RIP=0x{:x}, IO=0x{:x}",last.0,last.1,last.2)))
            })?;
            serial.flush().map_err(|e|e.to_string())?;
            if timed_out {return Err("Kernel wall deadline exceeded; no completed-boot claim".into());}
            result
        })
    }
    pub fn boot_kernel(plan:&crate::kernel::KernelPlan,serial:&mut dyn std::io::Write,trace:&mut crate::timing::BootTrace,start:std::time::Instant)->Result<(),String> {
        if plan.dma_failure.is_some() {return Err("DMA failure injection requires disk-create, not initramfs-only boot".into());}
        load_kernel_plan(plan,|partition,memory,surface| {
            if plan.apic_probe {return run_apic_os(partition,memory,serial,trace,start,(None,plan.bridge_probe,plan.dma_failure));}

            trace.vm_ready_ms=Some(start.elapsed().as_millis() as u64);
            let mut platform=crate::platform::EarlyPlatform::default();
            if plan.bridge_probe||plan.bridge_home_check {platform.bridge=Some(if let Some(apk)=&plan.bridge_apk {crate::bridge::Device::with_apk(apk)?} else if plan.bridge_home_check {crate::bridge::Device::with_home_check()} else {crate::bridge::Device::default()});}
            let mut net:Option<LiveNet>=None;
            if plan.net_probe {
                net=Some(LiveNet{context:MmioContext{partition:partition.0,ram:memory.address.cast(),bytes:memory.bytes,
                    device:MmioDevice::Net(crate::virtio_net_mmio::NetMmio::new()),error:None,translations:0,accesses:0},emulator:Emulator::new()?,backend:crate::virtio_net::Loopback::new()});
            }
            let mut audio:Option<LiveAudio>=None;
            if plan.audio_probe {
                audio=Some(LiveAudio{context:MmioContext{partition:partition.0,ram:memory.address.cast(),bytes:memory.bytes,
                    device:MmioDevice::Audio(crate::virtio_audio_mmio::AudioMmio::new()),error:None,translations:0,accesses:0},emulator:Emulator::new()?,backend:match plan.audio_backend {crate::kernel::AudioBackendKind::Null=>Box::new(crate::virtio_audio::NullBackend::new()),crate::kernel::AudioBackendKind::Real=>Box::new(crate::virtio_audio::RealBackend::new(44100))}});
            }
            let mut gpu:Option<LiveGpu>=None;
            if plan.gpu_probe {
                gpu=Some(LiveGpu{context:MmioContext{partition:partition.0,ram:memory.address.cast(),bytes:memory.bytes,
                    device:MmioDevice::Gpu(crate::virtio_gpu_mmio::GpuMmio::new()),error:None,translations:0,accesses:0},emulator:Emulator::new()?,backend:crate::virtio_gpu::ScanoutBackend{surface:crate::virtio_gpu::Surface{width:0,height:0,format:crate::virtio_gpu::FORMAT_XRGB8888,pitch:0,data:Vec::new()}}});
            }
            let mut null=LiveNull{context:MmioContext{partition:partition.0,ram:memory.address.cast(),bytes:memory.bytes,
                device:MmioDevice::Null,error:None,translations:0,accesses:0},emulator:Emulator::new()?};
            run_platform(partition,&mut platform,serial,trace,start,(if plan.bridge_apk.is_some() {RunGoal::NativeApk} else if plan.bridge_home_check {RunGoal::AndroidHome} else if plan.bridge_probe {RunGoal::NativeBridge} else {RunGoal::Diagnostic},surface.zip(plan.player.as_deref())),None,net.as_mut(),audio.as_mut(),gpu.as_mut(),Some(&mut null))
        })
    }
    pub fn boot_kernel_disk(plan:&crate::kernel::KernelPlan,serial:&mut dyn std::io::Write,trace:&mut crate::timing::BootTrace,start:std::time::Instant,base:&std::path::Path,cow:&std::path::Path,create:bool)->Result<(),String> {
        if plan.dma_failure.is_some()&&(!create||!plan.apic_probe||plan.cpu_count<2||plan.bridge_probe) {return Err("DMA failure test requires a NEW disposable APIC SMP overlay".into());}
        if plan.apic_probe&&(plan.android_init_probe||plan.bridge_apk.is_some()||plan.player.is_some()||plan.framebuffer_output.is_some()) {return Err("APIC disk requires headless Linux, no Android/APK/player".into());}
        let backend=if create {crate::overlay::Overlay::create(base,cow)?} else {crate::overlay::Overlay::open(base,cow)?};
        load_kernel_plan(plan,|partition,memory,surface| {
            if plan.apic_probe {return run_apic_os(partition,memory,serial,trace,start,(Some(backend),plan.bridge_probe,plan.dma_failure));}
            let capacity=backend.disk_bytes()/512;
            let mut disk=LiveBlock{context:MmioContext{partition:partition.0,ram:memory.address.cast(),bytes:memory.bytes,
                device:MmioDevice::Block(crate::virtio_mmio::BlockMmio::new(capacity,false)?),error:None,translations:0,accesses:0},emulator:Emulator::new()?,backend};
            let mut platform=crate::platform::EarlyPlatform::default();
            if plan.bridge_probe||plan.bridge_home_check {platform.bridge=Some(if let Some(apk)=&plan.bridge_apk {crate::bridge::Device::with_apk(apk)?} else if plan.bridge_home_check {crate::bridge::Device::with_home_check()} else {crate::bridge::Device::default()});}
            platform.pic.access(0x4d0,true,0x20)?;
            trace.vm_ready_ms=Some(start.elapsed().as_millis() as u64);
            let mut net:Option<LiveNet>=None;
            if plan.net_probe {
                net=Some(LiveNet{context:MmioContext{partition:partition.0,ram:memory.address.cast(),bytes:memory.bytes,
                    device:MmioDevice::Net(crate::virtio_net_mmio::NetMmio::new()),error:None,translations:0,accesses:0},emulator:Emulator::new()?,backend:crate::virtio_net::Loopback::new()});
            }
            let mut audio:Option<LiveAudio>=None;
            if plan.audio_probe {
                audio=Some(LiveAudio{context:MmioContext{partition:partition.0,ram:memory.address.cast(),bytes:memory.bytes,
                    device:MmioDevice::Audio(crate::virtio_audio_mmio::AudioMmio::new()),error:None,translations:0,accesses:0},emulator:Emulator::new()?,backend:match plan.audio_backend {crate::kernel::AudioBackendKind::Null=>Box::new(crate::virtio_audio::NullBackend::new()),crate::kernel::AudioBackendKind::Real=>Box::new(crate::virtio_audio::RealBackend::new(44100))}});
            }
            let mut gpu:Option<LiveGpu>=None;
            if plan.gpu_probe {
                gpu=Some(LiveGpu{context:MmioContext{partition:partition.0,ram:memory.address.cast(),bytes:memory.bytes,
                    device:MmioDevice::Gpu(crate::virtio_gpu_mmio::GpuMmio::new()),error:None,translations:0,accesses:0},emulator:Emulator::new()?,backend:crate::virtio_gpu::ScanoutBackend{surface:crate::virtio_gpu::Surface{width:0,height:0,format:crate::virtio_gpu::FORMAT_XRGB8888,pitch:0,data:Vec::new()}}});
            }
            let mut null=LiveNull{context:MmioContext{partition:partition.0,ram:memory.address.cast(),bytes:memory.bytes,
                device:MmioDevice::Null,error:None,translations:0,accesses:0},emulator:Emulator::new()?};
            run_platform(partition,&mut platform,serial,trace,start,(if plan.bridge_apk.is_some() {RunGoal::NativeApk} else if plan.bridge_home_check {RunGoal::AndroidHome} else if plan.bridge_probe {RunGoal::NativeBridge} else if plan.android_observe {RunGoal::AndroidObserve} else if plan.android_init_probe {RunGoal::AndroidInit} else {RunGoal::Diagnostic},surface.zip(plan.player.as_deref())),Some(&mut disk),net.as_mut(),audio.as_mut(),gpu.as_mut(),Some(&mut null))
        })
    }
    pub fn ata_pio_smoke(path:&std::path::Path)->Result<(),String> {ata_fixture(path,None)}
    pub fn ata_cow_smoke(path:&std::path::Path,overlay:&std::path::Path)->Result<(),String> {
        ata_fixture(path,Some(overlay))?;
        // vCPU and device are gone: prove persistence by a separate reopen.
        let mut disk=crate::overlay::Overlay::open(path,overlay)?;let mut data=[0;512];disk.read_sectors(1,&mut data)?;
        if data.as_slice()!=[0x17,0x5a].repeat(256) {return Err("Reopened overlay lost guest-written data".into());}Ok(())
    }
    /// Real firmware boot chain: the CPU starts in a real-mode firmware entry
    /// point (not direct kernel/sector entry). The firmware writes a POST marker,
    /// reads the boot sector from the ATA disk via real port I/O into 0x7c00, then
    /// transfers control to it. The boot sector writes a "booted" marker and HLTs.
    /// Verifies POST ran, the firmware performed real disk I/O, and the boot sector
    /// executed — a reset-vector firmware boot chain, not a direct loader.
    pub fn firmware_boot_smoke()->Result<(),String> {
        use crate::{cpu,ata::AtaPio};
        use std::io::Write;
        // Boot sector (disk sector 0): MOV EAX, "BOOT"; MOV [0x1b004], EAX; HLT; 0x55aa.
        let mut boot_sector=[0u8;512];
        let mut bs=Vec::new();
        bs.push(0xb8);bs.extend_from_slice(&0x544f4f42u32.to_le_bytes());
        bs.push(0xa3);bs.extend_from_slice(&0x1b004u32.to_le_bytes());
        bs.push(0xf4);
        boot_sector[..bs.len()].copy_from_slice(&bs);
        boot_sector[510]=0x55;boot_sector[511]=0xaa;
        // Build a 2-sector raw disk (boot sector + filler) and import to ZLAG.
        let dir=std::env::temp_dir().join(format!("nexus-fw-{}",std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e|e.to_string())?;
        let raw=dir.join("disk.raw");let zlag=dir.join("disk.zlag");
        {let mut f=std::fs::File::create(&raw).map_err(|e|e.to_string())?;
            f.write_all(&boot_sector).map_err(|e|e.to_string())?;
            f.write_all(&[0u8;512]).map_err(|e|e.to_string())?;}
        crate::image::import_raw(&raw,&zlag).map_err(|e|e.to_string())?;
        let ata=AtaPio::open(&zlag)?;
        // Firmware bytecode (protected 32-bit mode, placed at cpu::ENTRY).
        let out=|code:&mut Vec<u8>,port:u32,value:u8| {
            code.push(0xba);code.extend_from_slice(&port.to_le_bytes());
            code.push(0xb0);code.push(value);code.push(0xee);
        };
        let mut fw=Vec::new();
        // POST marker: MOV EAX, 0x504f5354; MOV [0x1b000], EAX.
        fw.push(0xb8);fw.extend_from_slice(&0x504f5354u32.to_le_bytes());
        fw.push(0xa3);fw.extend_from_slice(&0x1b000u32.to_le_bytes());
        // ATA: enable nIEN (0x3f6 bit1; IRQ14 not implemented), select drive 0
        // (LBA), read 1 sector (LBA 0), READ SECTORS command.
        out(&mut fw,0x3f6,2);
        for (port,value) in [(0x1f6u32,0xe0u8),(0x1f2,1),(0x1f3,0),(0x1f4,0),(0x1f5,0),(0x1f7,0x20)] {out(&mut fw,port,value);}
        // Read 256 words from 0x1f0 into 0x7c00.
        fw.push(0xba);fw.extend_from_slice(&0x1f0u32.to_le_bytes());     // MOV DX, 0x1f0 (4-byte imm)
        fw.push(0xbf);fw.extend_from_slice(&0x7c00u32.to_le_bytes());     // MOV EDI, 0x7c00
        fw.push(0xb9);fw.extend_from_slice(&256u32.to_le_bytes());
        let loop_start=fw.len();
        fw.push(0x66);fw.push(0xed);            // IN AX, DX (16-bit)
        fw.push(0x66);fw.push(0x89);fw.push(0x07);     // MOV [EDI], AX (16-bit)
        fw.push(0x83);fw.push(0xc7);fw.push(0x02); // ADD EDI, 2
        let jnz_pos=fw.len();
        fw.push(0xe2);fw.push(0); // LOOP rel8: dec ECX, jump if ECX!=0 (2 bytes)
        let jmp_pos=fw.len();
        fw.push(0xe9);fw.extend_from_slice(&[0,0,0,0]); // JMP rel32 -> 0x7c00
        // Patch JNZ back to loop_start (rel8, 2-byte instruction).
        let jnz_disp=(loop_start as i32)-(jnz_pos as i32+2);
        fw[jnz_pos+1]=(jnz_disp as i8) as u8;
        // Patch JMP to 0x7c00 (rel32). Displacement is relative to the JMP's
        // actual address, which is cpu::ENTRY + jmp_pos.
        let jmp_disp=(0x7c00i32)-(cpu::ENTRY as i32+jmp_pos as i32+5);
        fw[jmp_pos+1..jmp_pos+5].copy_from_slice(&jmp_disp.to_le_bytes());
        let fw_entry=cpu::ENTRY;
        let mut f=cpu::fixture(cpu::CpuMode::Protected32)?;
        f.ram[fw_entry..fw_entry+fw.len()].copy_from_slice(&fw);
        with_guest_at(&[],0,cpu::RAM_BYTES,|partition,memory| {
            unsafe {std::ptr::copy_nonoverlapping(f.ram.as_ptr(),memory.address.cast::<u8>(),f.ram.len());}
            configure_fixture(partition,&f)?;
            let mut platform=crate::platform::EarlyPlatform{ata:Some(ata),..Default::default()};
            let (result,timed_out)=bounded(partition,5000,|| {
                for _ in 0..2048 {
                    let context=run_once(partition)?;
                    match context.reason() {
                        8=>return Ok(Ok(())),
                        1=>{
                            // RAM memory access (the firmware writes the POST marker and
                            // stores the boot sector). Perform the write and advance RIP.
                            let mode=crate::cpu::CpuMode::from_exit(&context.0)?;
                            let rip=u64::from_le_bytes(context.0[32..40].try_into().unwrap());
                            let gpa=u64::from_le_bytes(context.0[48..56].try_into().unwrap());
                            let direction=context.0[56];
                            let size=context.0[57] as usize;
                            if size==0||size>8 {return Err(format!("Unsupported firmware memory access size {size}"));}
                            let end=gpa.checked_add(size as u64).ok_or("Firmware memory address overflow")?;
                            if end>memory.bytes as u64 {return Err(format!("Firmware memory access outside RAM: 0x{gpa:x}"));}
                            if direction==1 {
                                let ram=unsafe {std::slice::from_raw_parts_mut(memory.address.cast::<u8>(),memory.bytes)};
                                ram[gpa as usize..end as usize].copy_from_slice(&context.0[58..58+size]);
                            }
                            let next=mode.next_rip(rip,context.0[10]&15)?;
                            set_registers(partition,&[0x10],&[Register::scalar(next)])?;
                        }
                        2=>{
                            let io=crate::io::IoExit::decode(&context.0)?;
                            let mode=crate::cpu::CpuMode::from_exit(&context.0)?;
                            let rip=Register::scalar(mode.next_rip(io.rip,io.instruction_len)?);
                            if let Some(rax)=platform.access(io,0)? {set_registers(partition,&[0,0x10],&[Register::scalar(rax),rip])?;}
                            else {set_registers(partition,&[0x10],&[rip])?;}
                        }
                        reason=>return Err(format!("Unexpected firmware boot exit 0x{reason:x}")),
                    }
                }Ok(Err("Firmware boot exit budget exceeded".to_string()))
            })?;
            if timed_out {return Err("Firmware boot deadline exceeded".into());}result?;
            let ram=unsafe {std::slice::from_raw_parts(memory.address.cast::<u8>(),memory.bytes)};
            let post=u32::from_le_bytes(ram[0x1b000..0x1b004].try_into().unwrap());
            let booted=u32::from_le_bytes(ram[0x1b004..0x1b008].try_into().unwrap());
            if post!=0x504f5354 {return Err(format!("Firmware POST marker missing: 0x{post:x}"));}
            if booted!=0x544f4f42 {return Err(format!("Boot sector did not execute: 0x{booted:x}"));}
            let device=platform.ata.as_ref().ok_or("Missing ATA device")?;
            if device.sectors_read<1||device.commands<1 {return Err(format!("Firmware did not perform disk I/O: reads={} commands={}",device.sectors_read,device.commands));}
            Ok(())
        })?;
        let _=std::fs::remove_dir_all(&dir);
        Ok(())
    }
    fn ata_fixture(path:&std::path::Path,cow:Option<&std::path::Path>)->Result<(),String> {
        use crate::{cpu,ata::AtaPio,image::Image};
        let ata=if let Some(overlay)=cow {AtaPio::with_overlay(path,overlay,true)?} else {AtaPio::open(path)?};
        let mut image=Image::open(path).map_err(|e|e.to_string())?;
        if image.disk_bytes<1024 {return Err("ATA smoke needs at least two disk sectors".into());}
        let capacity=(image.disk_bytes/512) as u32;
        let mut expected=[0;512];image.read_sectors(1,&mut expected).map_err(|e|e.to_string())?;
        let mut f=cpu::fixture(cpu::CpuMode::Protected32)?;
        let mut code=Vec::new();
        let output=|code:&mut Vec<u8>,port:u32,value:u8| {
            code.push(0xba);code.extend_from_slice(&port.to_le_bytes());code.extend_from_slice(&[0xb0,value,0xee]);
        };
        let words=|code:&mut Vec<u8>,address:u32| {
            code.extend_from_slice(&[0xba,0xf0,1,0,0,0xbf]);code.extend_from_slice(&address.to_le_bytes());
            code.extend_from_slice(&[0xb9,0,1,0,0,0x66,0xed,0x66,0x89,7,0x83,0xc7,2,0xe2,0xf6]);
        };
        output(&mut code,0x3f6,2);output(&mut code,0x1f6,0xe0);output(&mut code,0x1f7,0xec);
        words(&mut code,0x1a000);
        if cow.is_some() {
            for (port,value) in [(0x1f2,1),(0x1f3,1),(0x1f4,0),(0x1f5,0),(0x1f7,0x30)] {output(&mut code,port,value);}
            code.extend_from_slice(&[0xba,0xf0,1,0,0,0xb9,0,1,0,0,0x66,0xb8,0x17,0x5a,0x66,0xef,0xe2,0xfc]);
            output(&mut code,0x1f7,0xe7);expected.copy_from_slice(&[0x17,0x5a].repeat(256));
        }
        for (port,value) in [(0x1f2,1),(0x1f3,1),(0x1f4,0),(0x1f5,0),(0x1f7,0x20)] {output(&mut code,port,value);}
        words(&mut code,0x1b000);
        output(&mut code,0x1f7,if cow.is_some() {0xca} else {0x30}); // Unsupported DMA or read-only write must ABRT.
        code.extend_from_slice(&[0xec,0xa2,0,0xc0,1,0,0xf4]); // Save status then HLT.
        f.ram[cpu::ENTRY..cpu::ENTRY+code.len()].copy_from_slice(&code);
        with_guest_at(&[],0,cpu::RAM_BYTES,|partition,memory| {
            unsafe {std::ptr::copy_nonoverlapping(f.ram.as_ptr(),memory.address.cast::<u8>(),f.ram.len());}
            configure_fixture(partition,&f)?;
            let mut platform=crate::platform::EarlyPlatform{ata:Some(ata),..Default::default()};
            let (result,timed_out)=bounded(partition,5000,|| {
                for _ in 0..2048 {
                    let context=run_once(partition)?;
                    match context.reason() {
                        8=>return Ok(Ok(())),
                        2=>{
                            let io=crate::io::IoExit::decode(&context.0)?;
                            let mode=crate::cpu::CpuMode::from_exit(&context.0)?;
                            let rip=Register::scalar(mode.next_rip(io.rip,io.instruction_len)?);
                            if let Some(rax)=platform.access(io,0)? {set_registers(partition,&[0,0x10],&[Register::scalar(rax),rip])?;}
                            else {set_registers(partition,&[0x10],&[rip])?;}
                        }
                        reason=>return Err(format!("Unexpected ATA fixture guest exit 0x{reason:x}")),
                    }
                }
                Ok(Err("ATA fixture exit budget exceeded".to_string()))
            })?;
            if timed_out {return Err("ATA fixture deadline exceeded".into());}result?;
            let ram=unsafe {std::slice::from_raw_parts(memory.address.cast::<u8>(),memory.bytes)};
            let observed_capacity=u32::from_le_bytes(ram[0x1a078..0x1a07c].try_into().unwrap());
            let device=platform.ata.as_ref().ok_or("Missing ATA fixture device")?;
            if ram[0x1b000..0x1b200]!=expected||observed_capacity!=capacity||ram[0x1c000]&1==0
                ||device.sectors_read!=1||device.commands!=if cow.is_some() {5} else {3}||device.sectors_written!=u64::from(cow.is_some())||scalar_register(partition,4)?!=cpu::STACK {
                return Err("ATA guest IDENTIFY/read/write-refusal/stack result mismatch".into());
            }
            Ok(())
        })
    }
    pub fn timer_irq_smoke()->Result<(),String> {
        timer_irq_mode(0x30)?;timer_irq_mode(0x38)
    }
    fn timer_irq_mode(control:u8)->Result<(),String> {
        use crate::cpu;
        let mut f=cpu::fixture(cpu::CpuMode::Protected32)?;
        f.idt_limit=0x107;
        // IRQ0 vector20 points at a protected-mode INC / EOI / IRETD handler.
        f.ram[cpu::IDT+0x100..cpu::IDT+0x108].copy_from_slice(&[0,0x81,8,0,0,0x8e,0,0]);
        // STI; NOP; HLT; CLI; HLT. The first halt must wake via an external IRQ.
        f.ram[cpu::ENTRY..cpu::ENTRY+5].copy_from_slice(&[0xfb,0x90,0xf4,0xfa,0xf4]);
        f.ram[0x8100..0x810b].copy_from_slice(&[0xff,0x05,0,0x90,1,0,0xb0,0x20,0xe6,0x20,0xcf]);
        with_guest_at(&[],0,cpu::RAM_BYTES,|partition,memory| {
            unsafe {std::ptr::copy_nonoverlapping(f.ram.as_ptr(),memory.address.cast::<u8>(),f.ram.len());}
            configure_fixture(partition,&f)?;
            let mut platform=crate::platform::EarlyPlatform::default();
            for (port,value) in [(0x20,0x11),(0x21,0x20),(0x21,4),(0x21,1),(0x21,0xfe)] {platform.pic.access(port,true,value)?;}
            // Long enough to exercise HLT wait, short enough for deterministic CI budgets.
            platform.pit0.access(0x43,true,control,0)?;
            platform.pit0.access(0x40,true,0,0)?;platform.pit0.access(0x40,true,0,0)?;
            let mut trace=crate::timing::BootTrace::default();let start=std::time::Instant::now();
            run_platform(partition,&mut platform,&mut std::io::sink(),&mut trace,start,(RunGoal::TimerFixture,None),None,None,None,None,None)?;
            let count=unsafe {std::ptr::read_unaligned(memory.address.cast::<u8>().add(cpu::INTERRUPT_COUNT).cast::<u32>())};
            if count!=1||trace.irq0_injections!=1||trace.halt_waits!=1||scalar_register(partition,4)?!=cpu::STACK {
                return Err(format!("Timer IRQ/HLT/IRETD mismatch count={count}, trace={}",trace.json()));
            }
            if platform.pic.pending_vector().is_some() {return Err("Timer IRQ remained pending after EOI".into());}
            Ok(())
        })
    }
    pub fn boot_test_sector(path:&std::path::Path)->Result<Vec<u8>,String> {
        let sector=crate::boot::read_test_sector(path)?;
        with_guest_at(&sector,0x7c00,1024*1024,|partition,_| {
            let mut bus=crate::io::TestBus::default();
            run_to_halt(partition,&mut bus)?;
            Ok(bus.console)
        })
    }
    pub fn cancel_smoke()->Result<(),String> {
        with_guest(&[0xeb,0xfe],|partition,memory| { // Deliberate no-exit infinite loop.
            // Error and panic must disconnect/join both cancellation workers before
            // this same partition can be reused or its handle can be destroyed.
            let expected="injected lifecycle error";
            if bounded::<()>(partition,1000,||Err(expected.into())).err().as_deref()!=Some(expected)
                ||timer_pump::<()>(partition,||Err(expected.into())).err().as_deref()!=Some(expected) {
                return Err("Cancellation worker error propagation failed".into());
            }
            for timer in [false,true] {
                let panic=std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if timer {let _=timer_pump::<()>(partition,||panic!("injected timer lifecycle panic"));}
                    else {let _=bounded::<()>(partition,1000,||panic!("injected watchdog lifecycle panic"));}
                }));
                if panic.is_ok() {return Err("Cancellation worker panic gate did not execute".into());}
            }
            let (context,canceled)=bounded(partition,250,||run_once(partition))?;
            if !canceled || context.reason()!=0x2001 {return Err(format!("Expected cancellation exit, got 0x{:x}",context.reason()));}
            // Prove cancellation did not destroy the vCPU: restart the SAME one.
            unsafe {std::ptr::copy_nonoverlapping(RAM_PROGRAM.as_ptr(),memory.address.cast::<u8>(),RAM_PROGRAM.len());}
            set_registers(partition,&[0x10,0],&[Register::scalar(0),Register::scalar(0)])?;
            run_to_halt(partition,&mut crate::io::TestBus::default())?;
            check_ram(partition,memory)
        })
    }

}
#[cfg(windows)]
pub use windows::{probe,cpu_smoke,io_smoke,cancel_smoke,boot_test_sector,protected_smoke,long_smoke,kernel_contract_smoke,boot_kernel};
#[cfg(not(windows))]
pub fn probe()->Result<(),String> { Err("Direct WHP backend requires Windows 10 x64".into()) }

#[cfg(not(windows))]
pub fn cpu_smoke()->Result<(),String> { Err("Direct WHP CPU test requires Windows 10 x64".into()) }

#[cfg(not(windows))]
pub fn io_smoke()->Result<(),String> {Err("Direct WHP port I/O requires Windows 10 x64".into())}
#[cfg(not(windows))]
pub fn cancel_smoke()->Result<(),String> {Err("Direct WHP cancellation requires Windows 10 x64".into())}

#[cfg(not(windows))]
pub fn boot_test_sector(_path:&std::path::Path)->Result<Vec<u8>,String> {Err("Direct WHP test-sector execution requires Windows 10 x64".into())}

#[cfg(not(windows))]
pub fn protected_smoke()->Result<(),String> {Err("Direct WHP protected-mode test requires Windows 10 x64".into())}
#[cfg(not(windows))]
pub fn long_smoke()->Result<(),String> {Err("Direct WHP long-mode test requires Windows 10 x64".into())}

#[cfg(not(windows))]
pub fn kernel_contract_smoke()->Result<(),String> {Err("Direct WHP kernel contract test requires Windows 10 x64".into())}
#[cfg(not(windows))]
pub fn boot_kernel(_plan:&crate::kernel::KernelPlan,_serial:&mut dyn std::io::Write,_trace:&mut crate::timing::BootTrace,_start:std::time::Instant)->Result<(),String> {Err("Direct WHP kernel execution requires Windows 10 x64".into())}

#[cfg(windows)]
pub use windows::timer_irq_smoke;
#[cfg(not(windows))]
pub fn timer_irq_smoke()->Result<(),String> {Err("Timer IRQ execution requires Windows x64 WHP".into())}

#[cfg(windows)]
pub use windows::ata_pio_smoke;
#[cfg(not(windows))]
pub fn ata_pio_smoke(_path:&std::path::Path)->Result<(),String> {Err("ATA guest execution requires Windows x64 WHP".into())}

#[cfg(windows)]
pub use windows::ata_cow_smoke;
#[cfg(not(windows))]
pub fn ata_cow_smoke(_base:&std::path::Path,_overlay:&std::path::Path)->Result<(),String> {Err("ATA COW execution requires Windows x64 WHP".into())}

#[cfg(windows)]
pub use windows::firmware_boot_smoke;
#[cfg(not(windows))]
pub fn firmware_boot_smoke()->Result<(),String> {Err("Firmware boot chain requires Windows x64 WHP".into())}

#[cfg(windows)]
pub use windows::mmio_smoke;
#[cfg(not(windows))]
pub fn mmio_smoke()->Result<(),String> {Err("WHP MMIO execution requires Windows x64".into())}

#[cfg(windows)]
pub use windows::net_mmio_smoke;
#[cfg(not(windows))]
pub fn net_mmio_smoke()->Result<(),String> {Err("WHP MMIO net execution requires Windows x64".into())}

#[cfg(windows)]
pub use windows::audio_mmio_smoke;
#[cfg(not(windows))]
pub fn audio_mmio_smoke()->Result<(),String> {Err("WHP MMIO audio execution requires Windows x64".into())}

#[cfg(windows)]
pub use windows::platform_mmio_smoke;
#[cfg(not(windows))]
pub fn platform_mmio_smoke()->Result<(),String> {Err("WHP MMIO platform execution requires Windows x64".into())}

#[cfg(windows)]
pub use windows::gpu_mmio_smoke;
#[cfg(not(windows))]
pub fn gpu_mmio_smoke()->Result<(),String> {Err("WHP MMIO gpu execution requires Windows x64".into())}

#[cfg(windows)]
pub use windows::boot_kernel_disk;
#[cfg(not(windows))]
pub fn boot_kernel_disk(_plan:&crate::kernel::KernelPlan,_serial:&mut dyn std::io::Write,_trace:&mut crate::timing::BootTrace,_start:std::time::Instant,_base:&std::path::Path,_cow:&std::path::Path,_create:bool)->Result<(),String> {Err("WHP VirtIO Linux boot requires Windows x64".into())}

#[cfg(windows)]
pub use windows::transition_smoke;
#[cfg(not(windows))]
pub fn transition_smoke()->Result<(),String> {Err("Guest mode transition execution requires Windows x64 WHP".into())}

#[cfg(windows)]
pub use windows::protection_smoke;
#[cfg(not(windows))]
pub fn protection_smoke()->Result<(),String> {Err("Guest page protection execution requires Windows x64 WHP".into())}

#[cfg(windows)]
pub use windows::smp_smoke;
#[cfg(not(windows))]
pub fn smp_smoke()->Result<(),String> {Err("SMP/APIC execution requires Windows x64 WHP".into())}

#[cfg(windows)]
pub use windows::host_os;
#[cfg(not(windows))]
pub fn host_os()->Result<crate::whp::HostOs,String> {Err("Host OS identity requires Windows".into())}
