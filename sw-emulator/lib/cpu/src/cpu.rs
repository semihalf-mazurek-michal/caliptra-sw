/*++

Licensed under the Apache-2.0 license.

File Name:

    cpu.rs

Abstract:

    File contains the implementation of Caliptra CPU.

--*/

use crate::csr_file::{Csr, CsrFile};
use crate::types::{RvInstr, RvMStatus};
use crate::xreg_file::{XReg, XRegFile};
use caliptra_emu_bus::{Bus, BusError, Clock};
use caliptra_emu_types::{RvAddr, RvData, RvException, RvSize};

pub type InstrTracer<'a> = dyn FnMut(u32, RvInstr) + 'a;

#[derive(PartialEq)]
pub enum WatchPtrKind {
    Read,
    Write,
}

pub struct WatchPtrHit {
    pub addr: u32,
    pub kind: WatchPtrKind,
}

pub struct WatchPtrCfg {
    pub write: Vec<u32>,
    pub read: Vec<u32>,
    pub hit: Option<WatchPtrHit>,
}

impl WatchPtrCfg {
    pub fn new() -> Self {
        Self {
            write: Vec::new(),
            read: Vec::new(),
            hit: None,
        }
    }
}

/// RISCV CPU
pub struct Cpu<TBus: Bus> {
    /// General Purpose register file
    xregs: XRegFile,

    /// Configuration & status registers
    csrs: CsrFile,

    // Program counter
    pc: RvData,

    /// The next program counter after the current instruction is finished executing.
    next_pc: RvData,

    // The bus the CPU uses to talk to memory and peripherals.
    pub bus: TBus,

    pub clock: Clock,

    // Track if Execution is in progress
    pub(crate) is_execute_instr: bool,

    // This is used to track watchpointers
    pub(crate) watch_ptr_cfg: WatchPtrCfg,
}

/// Cpu instruction step action
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum StepAction {
    /// Continue
    Continue,

    /// Break
    Break,

    /// Fatal
    Fatal,
}

impl<TBus: Bus> Cpu<TBus> {
    /// Default Program counter reset value
    const PC_RESET_VAL: RvData = 0;

    /// Create a new RISCV CPU
    pub fn new(bus: TBus, clock: Clock) -> Self {
        Self {
            xregs: XRegFile::new(),
            csrs: CsrFile::new(),
            pc: Self::PC_RESET_VAL,
            next_pc: Self::PC_RESET_VAL,
            bus,
            clock,
            is_execute_instr: false,
            watch_ptr_cfg: WatchPtrCfg::new(),
        }
    }

    /// Read the RISCV CPU Program counter
    ///
    ///  # Return
    ///
    ///  * `RvData` - PC value.
    pub fn read_pc(&self) -> RvData {
        self.pc
    }

    /// Write the RISCV CPU Program counter
    ///
    /// # Arguments
    ///
    /// * `pc` - Program counter value
    pub fn write_pc(&mut self, pc: RvData) {
        self.pc = pc;
    }

    /// Returns the next program counter after the current instruction is finished executing.
    pub fn next_pc(&self) -> RvData {
        self.next_pc
    }

    /// Set the next program counter after the current instruction is finished executing.
    ///
    /// Should only be set by instruction implementations.
    pub fn set_next_pc(&mut self, next_pc: RvData) {
        self.next_pc = next_pc;
    }

    /// Read the specified RISCV General Purpose Register
    ///
    /// # Arguments
    ///
    /// * `reg` - Register to read
    ///
    ///  # Return
    ///
    ///  * `RvData` - Register value if addr is valid; u32::MAX otherwise.
    ///
    /// # Error
    ///
    /// * `RvException` - Exception with cause `RvExceptionCause::IllegalRegister`
    pub fn read_xreg(&self, reg: XReg) -> Result<RvData, RvException> {
        self.xregs.read(reg)
    }

    /// Write the specified RISCV General Purpose Register
    ///
    /// # Arguments
    ///
    /// * `reg` - Register to write
    /// * `val` - Value to write
    ///
    /// # Error
    ///
    /// * `RvException` - Exception with cause `RvExceptionCause::IllegalRegister`
    pub fn write_xreg(&mut self, reg: XReg, val: RvData) -> Result<(), RvException> {
        self.xregs.write(reg, val)
    }

    /// Read the specified configuration status register
    ///
    /// # Arguments
    ///
    /// * `csr` - Configuration status register to read
    ///
    ///  # Return
    ///
    ///  * `RvData` - Register value
    ///
    /// # Error
    ///
    /// * `RvException` - Exception with cause `RvExceptionCause::IllegalRegister`
    pub fn read_csr(&self, csr: RvAddr) -> Result<RvData, RvException> {
        self.csrs.read(csr)
    }

    /// Write the specified Configuration status register
    ///
    /// # Arguments
    ///
    /// * `reg` - Configuration  status register to write
    /// * `val` - Value to write
    ///
    /// # Error
    ///
    /// * `RvException` - Exception with cause `RvExceptionCause::IllegalRegister`
    pub fn write_csr(&mut self, csr: RvAddr, val: RvData) -> Result<(), RvException> {
        self.csrs.write(csr, val)
    }

    /// Read from bus
    ///
    /// # Arguments
    ///
    /// * `size` - Size of the read
    /// * `addr` - Address to read from
    ///
    /// # Error
    ///
    /// * `RvException` - Exception with cause `RvExceptionCause::LoadAccessFault`
    ///                   or `RvExceptionCause::LoadAddrMisaligned`
    pub fn read_bus(&mut self, size: RvSize, addr: RvAddr) -> Result<RvData, RvException> {
        // Check if we are in step mode
        if self.is_execute_instr {
            self.watch_ptr_cfg.hit = match self.watch_ptr_cfg.read.contains(&addr) {
                true => Some(WatchPtrHit {
                    addr,
                    kind: WatchPtrKind::Read,
                }),
                false => None,
            }
        }

        match self.bus.read(size, addr) {
            Ok(val) => Ok(val),
            Err(exception) => match exception {
                BusError::LoadAccessFault => Err(RvException::load_access_fault(addr)),
                BusError::LoadAddrMisaligned => Err(RvException::load_addr_misaligned(addr)),
                BusError::StoreAccessFault => Err(RvException::store_access_fault(addr)),
                BusError::StoreAddrMisaligned => Err(RvException::store_addr_misaligned(addr)),
            },
        }
    }

    /// Write to bus
    ///
    /// # Arguments
    ///
    /// * `size` - Size of the read
    /// * `addr` - Address to read from
    /// * `val`  - Value to write
    ///
    /// # Error
    ///
    /// * `RvException` - Exception with cause `RvExceptionCause::StoreAccessFault`
    ///                   or `RvExceptionCause::StoreAddrMisaligned`
    pub fn write_bus(
        &mut self,
        size: RvSize,
        addr: RvAddr,
        val: RvData,
    ) -> Result<(), RvException> {
        // Check if we are in step mode
        if self.is_execute_instr {
            self.watch_ptr_cfg.hit = match self.watch_ptr_cfg.write.contains(&addr) {
                true => Some(WatchPtrHit {
                    addr,
                    kind: WatchPtrKind::Write,
                }),
                false => None,
            }
        }
        match self.bus.write(size, addr, val) {
            Ok(val) => Ok(val),
            Err(exception) => match exception {
                BusError::LoadAccessFault => Err(RvException::load_access_fault(addr)),
                BusError::LoadAddrMisaligned => Err(RvException::load_addr_misaligned(addr)),
                BusError::StoreAccessFault => Err(RvException::store_access_fault(addr)),
                BusError::StoreAddrMisaligned => Err(RvException::store_addr_misaligned(addr)),
            },
        }
    }

    /// Read instruction
    ///
    /// # Arguments
    ///
    /// * `size` - Size of the read
    /// * `addr` - Address to read from
    ///
    /// # Error
    ///
    /// * `RvException` - Exception with cause `RvExceptionCause::LoadAccessFault`
    ///                   or `RvExceptionCause::LoadAddrMisaligned`
    pub fn read_instr(&mut self, size: RvSize, addr: RvAddr) -> Result<RvData, RvException> {
        match size {
            RvSize::Byte => Err(RvException::instr_access_fault(addr)),
            _ => match self.bus.read(size, addr) {
                Ok(val) => Ok(val),
                Err(exception) => match exception {
                    BusError::LoadAccessFault => Err(RvException::instr_access_fault(addr)),
                    BusError::LoadAddrMisaligned => Err(RvException::instr_addr_misaligned(addr)),
                    BusError::StoreAccessFault => Err(RvException::store_access_fault(addr)),
                    BusError::StoreAddrMisaligned => Err(RvException::store_addr_misaligned(addr)),
                },
            },
        }
    }

    /// Step a single instruction
    ///
    /// # Error
    ///
    /// * `RvException` - Exception
    pub fn step(&mut self, instr_tracer: Option<&mut InstrTracer>) -> StepAction {
        self.clock.increment_and_poll(1, &mut self.bus);
        match self.exec_instr(instr_tracer) {
            Ok(result) => result,
            Err(exception) => self.handle_exception(exception),
        }
    }

    /// Handle synchronous exception
    ///
    /// # Error
    ///
    /// * `RvException` - Exception
    fn handle_exception(&mut self, exception: RvException) -> StepAction {
        let ret = self.handle_trap(
            false,
            self.read_pc(),
            exception.cause().into(),
            exception.info(),
        );
        match ret {
            Ok(_) => StepAction::Continue,
            Err(_) => StepAction::Fatal,
        }
    }

    /// Handle synchronous & asynchronous trap
    ///
    /// # Error
    ///
    /// * `RvException` - Exception
    fn handle_trap(
        &mut self,
        _intr: bool,
        pc: RvAddr,
        cause: u32,
        info: u32,
    ) -> Result<(), RvException> {
        // TODO: Implement Interrupt Handling
        // 1. Support for vectored asynchronous interrupts
        // 2. Veer fast external interrupt support
        assert!(!_intr);

        self.write_csr(Csr::MEPC, pc)?;
        self.write_csr(Csr::MCAUSE, cause)?;
        self.write_csr(Csr::MTVAL, info)?;

        let mut status = RvMStatus(self.read_csr(Csr::MSTATUS)?);
        status.set_mpie(status.mie());
        status.set_mie(0);
        self.write_csr(Csr::MSTATUS, status.0)?;

        let next_pc = self.read_csr(Csr::MTVEC)? & !0b11;
        self.write_pc(next_pc);
        println!("{:x}", next_pc);
        Ok(())
    }

    //// Append WatchPointer
    pub fn add_watchptr(&mut self, addr: u32, len: u32, kind: WatchPtrKind) {
        for addr in addr..(addr + len) {
            match kind {
                WatchPtrKind::Read => self.watch_ptr_cfg.read.push(addr),
                WatchPtrKind::Write => self.watch_ptr_cfg.write.push(addr),
            }
        }
    }

    //// Remove WatchPointer
    pub fn remove_watchptr(&mut self, addr: u32, len: u32, kind: WatchPtrKind) {
        let watch_ptr = match kind {
            WatchPtrKind::Read => &mut self.watch_ptr_cfg.read,
            WatchPtrKind::Write => &mut self.watch_ptr_cfg.write,
        };
        watch_ptr.retain(|&x| -> bool { (x < addr) || (x > (addr + len)) });
    }

    //// Get WatchPointer
    pub fn get_watchptr_hit(&self) -> Option<&WatchPtrHit> {
        self.watch_ptr_cfg.hit.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use caliptra_emu_bus::{testing::FakeBus, DynamicBus, Rom, Timer};

    #[test]
    fn test_new() {
        let cpu = Cpu::new(DynamicBus::new(), Clock::new());
        assert_eq!(cpu.read_pc(), 0);
    }

    #[test]
    fn test_pc() {
        let mut cpu = Cpu::new(DynamicBus::new(), Clock::new());
        cpu.write_pc(0xFF);
        assert_eq!(cpu.read_pc(), 0xFF);
    }

    #[test]
    fn test_xreg() {
        let mut cpu = Cpu::new(DynamicBus::new(), Clock::new());
        for reg in 1..32u32 {
            assert_eq!(cpu.write_xreg(reg.into(), 0xFF).ok(), Some(()));
            assert_eq!(cpu.read_xreg(reg.into()).ok(), Some(0xFF));
        }
    }

    #[test]
    fn test_bus_poll() {
        const RV32_NO_OP: u32 = 0x00000013;

        let clock = Clock::new();
        let timer = Timer::new(&clock);
        let mut bus = DynamicBus::new();

        let rom = Rom::new(
            std::iter::repeat(RV32_NO_OP)
                .take(256)
                .flat_map(u32::to_le_bytes)
                .collect(),
        );
        bus.attach_dev("ROM", 0..=0x3ff, Box::new(rom)).unwrap();

        let fake_bus = FakeBus::new();
        let fake_bus_log = fake_bus.log.clone();
        bus.attach_dev("FAKE", 0x2000..=0x3000, Box::new(fake_bus))
            .unwrap();

        let mut action0 = Some(timer.schedule_poll_in(31));

        let mut cpu = Cpu::new(bus, clock);
        for i in 0..30 {
            assert_eq!(cpu.clock.now(), i);
            assert_eq!(cpu.step(None), StepAction::Continue);
        }
        assert_eq!(fake_bus_log.take(), "");
        assert!(!timer.fired(&mut action0));

        assert_eq!(cpu.step(None), StepAction::Continue);
        assert_eq!(fake_bus_log.take(), "poll()\n");
        assert!(timer.fired(&mut action0));

        assert_eq!(cpu.read_pc(), 31 * 4);
    }
}
