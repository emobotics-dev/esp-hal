use core::{
    cell::{Cell, UnsafeCell},
    cmp::min,
    mem::{ManuallyDrop, MaybeUninit},
    ptr::NonNull,
    sync::atomic::{Ordering, fence},
};

#[cfg(feature = "unstable")]
use embedded_hal::spi::{ErrorType, SpiBus};

use super::*;
use crate::{
    dma::{
        Channel,
        DmaChannelFor,
        DmaDescriptor,
        DmaEligible,
        DmaRxBuf,
        DmaRxBuffer,
        DmaRxInterrupt,
        DmaTxBuf,
        DmaTxBuffer,
        DmaTxInterrupt,
        PeripheralDmaChannel,
        asynch::DmaRxFuture,
    },
    private::DropGuard,
    soc::is_slice_in_dram,
    spi::DmaError,
};
#[cfg(esp32)]
use crate::dma::prepare_for_tx_with_pad;
// esp32 uses the padded variant (above); other chips use the plain zero-copy
// path, so gate the import to match its single use site below.
#[cfg(not(esp32))]
use crate::dma::prepare_for_tx;

const MAX_DMA_SIZE: usize = 32736;

/// Async transfers cancelled mid-flight — non-zero means an SD op timed out.
pub static CANCEL_QUIESCE_HITS: portable_atomic::AtomicU32 = portable_atomic::AtomicU32::new(0);
/// Worst spin seen waiting for a cancelled engine to stop.
pub static CANCEL_QUIESCE_MAX_POLLS: portable_atomic::AtomicU32 =
    portable_atomic::AtomicU32::new(0);
/// Cancels where the engine never stopped and the DMA had to be reset.
pub static CANCEL_QUIESCE_RESETS: portable_atomic::AtomicU32 = portable_atomic::AtomicU32::new(0);

/// Where `wait_for_idle_async` is: 1 = RX future, 2 = TransferDone, 0 = out.
pub static SPI_WAIT_PHASE: portable_atomic::AtomicU32 = portable_atomic::AtomicU32::new(0);
/// Bumped on every [`SPI_WAIT_PHASE`] change; frozen means parked.
pub static SPI_WAIT_SEQ: portable_atomic::AtomicU32 = portable_atomic::AtomicU32::new(0);
/// TransferDone polls. At phase 2: frozen = lost wakeup, rising = the
/// transfer is polled but never completes.
pub static FUT_POLLS: portable_atomic::AtomicU32 = portable_atomic::AtomicU32::new(0);

/// RX DMA descriptor faults recovered in `wait_for_idle_async` (esp-hal #491).
pub static RX_DSCR_FAULTS: portable_atomic::AtomicU32 = portable_atomic::AtomicU32::new(0);
/// esp32 `usr`-stuck faults recovered after `TransferDone` (esp-hal #491).
pub static USR_STUCK_RECOVERIES: portable_atomic::AtomicU32 = portable_atomic::AtomicU32::new(0);

impl<'d> Spi<'d, Blocking> {
    #[doc_replace(
        "dma_channel" => {
            cfg(any(esp32, esp32s2)) => "DMA_SPI2",
            _ => "DMA_CH0",
        }
    )]
    /// Configures the SPI instance to use DMA with the specified channel.
    ///
    /// This method prepares the SPI instance for DMA transfers using SPI
    /// and returns an instance of `SpiDma` that supports DMA
    /// operations.
    /// ```rust, no_run
    /// # {before_snippet}
    /// use esp_hal::{
    ///     dma::{DmaRxBuf, DmaTxBuf},
    ///     dma_buffers,
    ///     spi::{
    ///         Mode,
    ///         master::{Config, Spi},
    ///     },
    /// };
    /// let (rx_buffer, rx_descriptors, tx_buffer, tx_descriptors) = dma_buffers!(32000);
    ///
    /// let dma_rx_buf = DmaRxBuf::new(rx_descriptors, rx_buffer)?;
    /// let dma_tx_buf = DmaTxBuf::new(tx_descriptors, tx_buffer)?;
    ///
    /// let mut spi = Spi::new(
    ///     peripherals.SPI2,
    ///     Config::default()
    ///         .with_frequency(Rate::from_khz(100))
    ///         .with_mode(Mode::_0),
    /// )?
    /// .with_dma(peripherals.__dma_channel__)
    /// .with_buffers(dma_rx_buf, dma_tx_buf);
    /// # {after_snippet}
    /// ```
    #[instability::unstable]
    pub fn with_dma(self, channel: impl DmaChannelFor<AnySpi<'d>>) -> SpiDma<'d, Blocking> {
        SpiDma::new(self, channel.degrade())
    }
}

#[doc_replace(
    "dma_channel" => {
        cfg(any(esp32, esp32s2)) => "DMA_SPI2",
        _ => "DMA_CH0",
    }
)]
/// A DMA capable SPI instance.
///
/// Using `SpiDma` is not recommended unless you wish
/// to manage buffers yourself. It's recommended to use
/// [`SpiDmaBus`] via `with_buffers` to get access
/// to a DMA capable SPI bus that implements the
/// embedded-hal traits.
/// ```rust, no_run
/// # {before_snippet}
/// use esp_hal::{
///     dma::{DmaRxBuf, DmaTxBuf},
///     dma_buffers,
///     spi::{
///         Mode,
///         master::{Config, Spi},
///     },
/// };
/// let (rx_buffer, rx_descriptors, tx_buffer, tx_descriptors) = dma_buffers!(32000);
///
/// let dma_rx_buf = DmaRxBuf::new(rx_descriptors, rx_buffer)?;
/// let dma_tx_buf = DmaTxBuf::new(tx_descriptors, tx_buffer)?;
///
/// let mut spi = Spi::new(
///     peripherals.SPI2,
///     Config::default()
///         .with_frequency(Rate::from_khz(100))
///         .with_mode(Mode::_0),
/// )?
/// .with_dma(peripherals.__dma_channel__)
/// .with_buffers(dma_rx_buf, dma_tx_buf);
/// #
/// # {after_snippet}
/// ```
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct SpiDma<'d, Dm>
where
    Dm: DriverMode,
{
    spi: SpiWrapper<'d>,
    pub(crate) channel: Channel<Dm, PeripheralDmaChannel<AnySpi<'d>>>,
}

impl<Dm> crate::private::Sealed for SpiDma<'_, Dm> where Dm: DriverMode {}

impl<'d> SpiDma<'d, Blocking> {
    /// Converts the SPI instance into async mode.
    #[instability::unstable]
    pub fn into_async(self) -> SpiDma<'d, Async> {
        self.spi
            .set_interrupt_handler(self.spi.info().async_handler);
        SpiDma {
            spi: self.spi,
            channel: self.channel.into_async(),
        }
    }

    pub(super) fn new(
        spi_driver: Spi<'d, Blocking>,
        channel: PeripheralDmaChannel<AnySpi<'d>>,
    ) -> Self {
        let spi = spi_driver.spi;

        let channel = Channel::new(channel);
        channel.runtime_ensure_compatible(&spi.spi);

        for_each_spi_master!((all $($inst:tt),*) => {
            const SPI_NUM: usize = 0 $(+ { stringify!($inst); 1 })*;
        };);
        let id = if spi.info() == unsafe { crate::peripherals::SPI2::steal().info() } {
            0
        } else {
            1
        };

        let state = spi.spi.dma_state();

        state.tx_transfer_in_progress.set(false);
        state.rx_transfer_in_progress.set(false);

        static mut TX_DESCRIPTORS: [[DmaDescriptor; 1]; SPI_NUM] =
            [[DmaDescriptor::EMPTY]; SPI_NUM];
        static mut RX_DESCRIPTORS: [[DmaDescriptor; 1]; SPI_NUM] =
            [[DmaDescriptor::EMPTY]; SPI_NUM];

        let empty_rx_buffer = unwrap!(DmaRxBuf::new(unsafe { &mut RX_DESCRIPTORS[id] }, &mut []));

        cfg_if::cfg_if! {
            if #[cfg(all(esp32, spi_address_workaround))] {
                static mut BUFFERS: [[u32; 1]; SPI_NUM] = [[0]; SPI_NUM];
                let buffer = crate::dma::as_mut_byte_array!(BUFFERS[id], 4);
                let empty_tx_buffer = unwrap!(DmaTxBuf::new(unsafe { &mut TX_DESCRIPTORS[id] }, buffer));
            } else {
                let empty_tx_buffer = unwrap!(DmaTxBuf::new(unsafe { &mut TX_DESCRIPTORS[id] }, &mut []));
            }
        }

        // The buffers must be set up when creating the driver.
        unsafe { (&mut *state.empty_tx_buffer.get()).write(empty_tx_buffer) };
        unsafe { (&mut *state.empty_rx_buffer.get()).write(empty_rx_buffer) };

        Self { spi, channel }
    }

    /// Listen for the given interrupts
    #[instability::unstable]
    pub fn listen(&mut self, interrupts: impl Into<EnumSet<SpiInterrupt>>) {
        self.driver().enable_listen(interrupts.into(), true);
    }

    /// Unlisten the given interrupts
    #[instability::unstable]
    pub fn unlisten(&mut self, interrupts: impl Into<EnumSet<SpiInterrupt>>) {
        self.driver().enable_listen(interrupts.into(), false);
    }

    /// Gets asserted interrupts
    #[instability::unstable]
    pub fn interrupts(&mut self) -> EnumSet<SpiInterrupt> {
        self.driver().interrupts()
    }

    /// Listen for the given DMA receive interrupts.
    ///
    /// Separate from [`SpiDma::listen`], which covers the SPI peripheral's own
    /// interrupts: a chained receive must complete on `IN_SUC_EOF` -- the
    /// signal that means the bytes are in memory -- and not on the SPI
    /// transaction being over, which is earlier.
    #[instability::unstable]
    pub fn listen_dma_rx(&mut self, interrupts: impl Into<EnumSet<DmaRxInterrupt>>) {
        self.channel.rx.listen_in(interrupts);
    }

    /// Stop listening for the given DMA receive interrupts.
    #[instability::unstable]
    pub fn unlisten_dma_rx(&mut self, interrupts: impl Into<EnumSet<DmaRxInterrupt>>) {
        self.channel.rx.unlisten_in(interrupts);
    }

    /// Gets asserted DMA receive interrupts.
    #[instability::unstable]
    pub fn pending_dma_rx(&mut self) -> EnumSet<DmaRxInterrupt> {
        self.channel.rx.pending_in_interrupts()
    }

    /// Clears the given DMA receive interrupts.
    ///
    /// A handler that re-arms must clear first: an interrupt left asserted from
    /// the previous transfer makes the next one look complete the moment it is
    /// armed.
    #[instability::unstable]
    pub fn clear_dma_rx(&mut self, interrupts: impl Into<EnumSet<DmaRxInterrupt>>) {
        self.channel.rx.clear_in(interrupts);
    }

    /// Listen for the given DMA transmit interrupts.
    #[instability::unstable]
    pub fn listen_dma_tx(&mut self, interrupts: impl Into<EnumSet<DmaTxInterrupt>>) {
        self.channel.tx.listen_out(interrupts);
    }

    /// Stop listening for the given DMA transmit interrupts.
    #[instability::unstable]
    pub fn unlisten_dma_tx(&mut self, interrupts: impl Into<EnumSet<DmaTxInterrupt>>) {
        self.channel.tx.unlisten_out(interrupts);
    }

    /// Gets asserted DMA transmit interrupts.
    #[instability::unstable]
    pub fn pending_dma_tx(&mut self) -> EnumSet<DmaTxInterrupt> {
        self.channel.tx.pending_out_interrupts()
    }

    /// Clears the given DMA transmit interrupts.
    #[instability::unstable]
    pub fn clear_dma_tx(&mut self, interrupts: impl Into<EnumSet<DmaTxInterrupt>>) {
        self.channel.tx.clear_out(interrupts);
    }

    /// Whether the transfer in flight has finished, without waiting for it.
    ///
    /// True once both DMA halves are done and the SPI peripheral is idle. This
    /// is the check an interrupt handler makes; [`SpiDmaTransfer::wait`] is the
    /// same condition with a spin loop around it, which a handler cannot
    /// afford.
    #[instability::unstable]
    pub fn transfer_done(&self) -> bool {
        self.is_done()
    }

    /// Arm a half-duplex read and return immediately, without a transfer
    /// object and without waiting.
    ///
    /// This is the primitive an interrupt-chained driver needs: the completion
    /// arrives as an interrupt, and the next transaction is armed from inside
    /// that handler, so there is nothing to hold a waiter or a `Future`. The
    /// buffer stays owned by the caller, which keeps the validation
    /// [`DmaRxBuf::new`] performs.
    ///
    /// Pair every call with [`SpiDma::take_transfer`] once
    /// [`SpiDma::transfer_done`] is true.
    ///
    /// # Safety
    ///
    /// The caller must not access `buffer`'s contents, drop it, or arm another
    /// transfer until [`SpiDma::take_transfer`] has returned `true`. Moving the
    /// buffer is allowed. Nothing here checks that a transfer is already in
    /// flight -- the caller's state machine is what guarantees it.
    ///
    /// # Errors
    ///
    /// [`Error`] if the requested transfer cannot be programmed.
    #[cfg_attr(place_spi_master_driver_in_ram, ram)]
    #[instability::unstable]
    pub unsafe fn arm_half_duplex_read(
        &mut self,
        data_mode: DataMode,
        cmd: Command,
        address: Address,
        dummy: u8,
        bytes_to_read: usize,
        buffer: &mut impl DmaRxBuffer,
    ) -> Result<(), Error> {
        unsafe {
            self.start_half_duplex_read(data_mode, cmd, address, dummy, bytes_to_read, buffer)
        }
    }

    /// Arm a half-duplex write and return immediately. The mirror of
    /// [`SpiDma::arm_half_duplex_read`]; the same safety contract applies.
    ///
    /// # Safety
    ///
    /// See [`SpiDma::arm_half_duplex_read`].
    ///
    /// # Errors
    ///
    /// [`Error`] if the requested transfer cannot be programmed.
    #[cfg_attr(place_spi_master_driver_in_ram, ram)]
    #[instability::unstable]
    pub unsafe fn arm_half_duplex_write(
        &mut self,
        data_mode: DataMode,
        cmd: Command,
        address: Address,
        dummy: u8,
        bytes_to_write: usize,
        buffer: &mut impl DmaTxBuffer,
    ) -> Result<(), Error> {
        unsafe {
            self.start_half_duplex_write(data_mode, cmd, address, dummy, bytes_to_write, buffer)
        }
    }

    /// Release an armed transfer if it has finished, returning whether it had.
    ///
    /// The non-blocking half of [`SpiDmaTransfer::wait`]: on `true` the
    /// in-flight state is cleared with an acquire fence, so the buffer's
    /// contents are visible to the CPU and the buffer may be read, reused or
    /// dropped. On `false` nothing changes and the transfer is still running.
    #[instability::unstable]
    pub fn take_transfer(&mut self) -> bool {
        if !self.is_done() {
            return false;
        }
        self.dma_driver().state.rx_transfer_in_progress.set(false);
        self.dma_driver().state.tx_transfer_in_progress.set(false);
        fence(Ordering::Acquire);
        true
    }

    /// Resets asserted interrupts
    #[instability::unstable]
    pub fn clear_interrupts(&mut self, interrupts: impl Into<EnumSet<SpiInterrupt>>) {
        self.driver().clear_interrupts(interrupts.into());
    }

    #[cfg_attr(
        not(multi_core),
        doc = "Registers an interrupt handler for the peripheral."
    )]
    #[cfg_attr(
        multi_core,
        doc = "Registers an interrupt handler for the peripheral on the current core."
    )]
    #[doc = ""]
    /// Note that this will replace any previously registered interrupt
    /// handlers.
    ///
    /// You can restore the default/unhandled interrupt handler by using
    /// [crate::interrupt::DEFAULT_INTERRUPT_HANDLER]
    ///
    /// # Panics
    ///
    /// Panics if passed interrupt handler is invalid (e.g. has priority
    /// `None`)
    #[instability::unstable]
    pub fn set_interrupt_handler(&mut self, handler: InterruptHandler) {
        self.spi.set_interrupt_handler(handler);
    }
}

impl<'d> SpiDma<'d, Async> {
    /// Converts the SPI instance into blocking mode.
    #[instability::unstable]
    pub fn into_blocking(self) -> SpiDma<'d, Blocking> {
        self.spi.disable_peri_interrupt_on_all_cores();
        SpiDma {
            spi: self.spi,
            channel: self.channel.into_blocking(),
        }
    }

    async fn wait_for_idle_async(&mut self) {
        fn wphase(p: u32) {
            SPI_WAIT_PHASE.store(p, Ordering::Relaxed);
            SPI_WAIT_SEQ.fetch_add(1, Ordering::Relaxed);
        }
        if self.dma_driver().state.rx_transfer_in_progress.get() {
            wphase(1);
            let rx_result = DmaRxFuture::new(&mut self.channel.rx).await;
            if rx_result.is_err() {
                // RX descriptor fault: `usr` stays stuck and no TransferDone
                // ever arrives, so recover the peripheral rather than wait
                // forever. Bad/short data becomes a CRC error sdspi retries.
                // esp-hal #491 / docs/spi-dma-and-wakeup.md §7.
                self.dma_driver().reset_dma();
                self.cancel_transfer();
                RX_DSCR_FAULTS.fetch_add(1, Ordering::Relaxed);
                fence(Ordering::Acquire);
                return;
            }
            self.dma_driver().state.rx_transfer_in_progress.set(false);
        }

        struct Fut {
            driver: Driver,
            // Post-TransferDone busy-spin counter (esp32/esp32s2 only) — see
            // BUSY_POLL_BUDGET. esp32s3+ never re-polls so it has no field.
            #[cfg(any(esp32, esp32s2))]
            busy_polls: u32,
        }
        impl Fut {
            const DONE_EVENTS: EnumSet<SpiInterrupt> =
                enumset::enum_set!(SpiInterrupt::TransferDone);
            // Bound the post-DONE `usr` re-poll: unbounded self-waking pins
            // the interrupt executor and masks the timer, so `with_timeout`
            // never fires and the core wedges silently (esp-hal #491).
            #[cfg(any(esp32, esp32s2))]
            const BUSY_POLL_BUDGET: u32 = 50_000;
        }
        impl Future for Fut {
            type Output = ();

            fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                let this = self.get_mut();
                FUT_POLLS.fetch_add(1, Ordering::Relaxed);
                if !this.driver.interrupts().is_disjoint(Self::DONE_EVENTS) {
                    #[cfg(any(esp32, esp32s2))]
                    // Need to poll for done-ness even after interrupt fires —
                    // bounded (see BUSY_POLL_BUDGET) so a stuck `usr` can't
                    // spin forever and wedge the core.
                    if this.driver.busy() {
                        this.busy_polls = this.busy_polls.saturating_add(1);
                        if this.busy_polls < Self::BUSY_POLL_BUDGET {
                            cx.waker().wake_by_ref();
                            return Poll::Pending;
                        }
                        // Budget exhausted: fall through to Ready. The caller
                        // (wait_for_idle_async) sees still-busy and recovers.
                    }

                    this.driver.clear_interrupts(Self::DONE_EVENTS);
                    return Poll::Ready(());
                }

                this.driver.state.waker.register(cx.waker());
                this.driver.enable_listen(Self::DONE_EVENTS, true);
                Poll::Pending
            }
        }
        impl Drop for Fut {
            fn drop(&mut self) {
                self.driver.enable_listen(Self::DONE_EVENTS, false);
            }
        }

        if !self.is_done() {
            wphase(2);
            Fut {
                driver: self.driver(),
                #[cfg(any(esp32, esp32s2))]
                busy_polls: 0,
            }
            .await;
        }

        // esp32/esp32s2: if `usr` is still stuck after the bounded post-DONE
        // wait, the SPI peripheral wedged (PDMA usr-stuck fault, esp-hal #491).
        // Recover like the RX-descriptor-fault path above so the next op isn't
        // blocked — the wedged op returns short/bad data → CRC error → sdspi
        // retries → recoverable, never a silent infinite hang.
        #[cfg(any(esp32, esp32s2))]
        if self.driver().busy() {
            self.dma_driver().reset_dma();
            self.cancel_transfer();
            USR_STUCK_RECOVERIES.fetch_add(1, Ordering::Relaxed);
            fence(Ordering::Acquire);
            return;
        }

        if self.dma_driver().state.tx_transfer_in_progress.get() {
            // In case DMA TX buffer is bigger than what the SPI consumes, stop the DMA.
            if !self.channel.tx.is_done() {
                self.channel.tx.stop_transfer();
            }
            self.dma_driver().state.tx_transfer_in_progress.set(false);
        }

        wphase(0);
        // The caller reads the DMA-written buffer next; this orders those
        // loads after the done-check, as every other completion path does.
        fence(Ordering::Acquire);
    }
}

impl<Dm> core::fmt::Debug for SpiDma<'_, Dm>
where
    Dm: DriverMode + core::fmt::Debug,
{
    /// Formats the `SpiDma` instance for debugging purposes.
    ///
    /// This method returns a debug struct with the name "SpiDma" without
    /// exposing internal details.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SpiDma").field("spi", &self.spi).finish()
    }
}

#[instability::unstable]
impl crate::interrupt::InterruptConfigurable for SpiDma<'_, Blocking> {
    /// Sets the interrupt handler
    ///
    /// Interrupts are not enabled at the peripheral level here.
    fn set_interrupt_handler(&mut self, handler: InterruptHandler) {
        self.set_interrupt_handler(handler);
    }
}

impl<Dm> SpiDma<'_, Dm>
where
    Dm: DriverMode,
{
    fn spi(&self) -> &SpiWrapper<'_> {
        &self.spi
    }

    fn driver(&self) -> Driver {
        Driver {
            info: self.spi.info(),
            state: self.spi.state(),
        }
    }

    fn dma_driver(&self) -> DmaDriver {
        DmaDriver {
            driver: self.driver(),
            dma_peripheral: self.spi().dma_peripheral(),
            state: self.spi().dma_state(),
        }
    }

    /// Spin budget for `cancel_and_quiesce`: far above a burst wind-down, so
    /// it only ever bounds the `usr`-stuck fault.
    const CANCEL_QUIESCE_POLLS: u32 = 50_000;

    fn is_done(&self) -> bool {
        if self.driver().busy() {
            return false;
        }
        if self.dma_driver().state.rx_transfer_in_progress.get() {
            // If this is an asymmetric transfer and the RX side is smaller, the RX channel
            // will never be "done" as it won't have enough descriptors/buffer to receive
            // the EOF bit from the SPI. So instead the RX channel will hit
            // a "descriptor empty" which means the DMA is written as much
            // of the received data as possible into the buffer and
            // discarded the rest. The user doesn't care about this discarded data.

            if !self.channel.rx.is_done() && !self.channel.rx.has_dscr_empty_error() {
                return false;
            }
        }
        true
    }

    fn wait_for_idle(&mut self) {
        while !self.is_done() {
            // Wait for the SPI to become idle
        }
        self.dma_driver().state.rx_transfer_in_progress.set(false);
        self.dma_driver().state.tx_transfer_in_progress.set(false);
        fence(Ordering::Acquire);
    }

    /// # Safety:
    ///
    /// The caller must ensure to not access the buffer contents while the
    /// transfer is in progress. Moving the buffer itself is allowed.
    #[cfg_attr(place_spi_master_driver_in_ram, ram)]
    unsafe fn start_transfer_dma<RX: DmaRxBuffer, TX: DmaTxBuffer>(
        &mut self,
        full_duplex: bool,
        bytes_to_read: usize,
        bytes_to_write: usize,
        rx_buffer: &mut RX,
        tx_buffer: &mut TX,
    ) -> Result<(), Error> {
        if bytes_to_read > MAX_DMA_SIZE || bytes_to_write > MAX_DMA_SIZE {
            return Err(Error::MaxDmaTransferSizeExceeded);
        }

        self.dma_driver()
            .state
            .rx_transfer_in_progress
            .set(bytes_to_read > 0);
        self.dma_driver()
            .state
            .tx_transfer_in_progress
            .set(bytes_to_write > 0);
        unsafe {
            self.dma_driver().start_transfer_dma(
                full_duplex,
                bytes_to_read,
                bytes_to_write,
                rx_buffer,
                tx_buffer,
                &mut self.channel,
            )
        }
    }

    /// # Safety:
    ///
    /// The caller must ensure that the buffers are not accessed while the
    /// transfer is in progress. Moving the buffers is allowed.
    #[cfg(all(esp32, spi_address_workaround))]
    unsafe fn set_up_address_workaround(
        &mut self,
        cmd: Command,
        address: Address,
        dummy: u8,
    ) -> Result<(), Error> {
        if dummy > 0 {
            // FIXME: https://github.com/esp-rs/esp-hal/issues/2240
            error!("Dummy bits are not supported when there is no data to write");
            return Err(Error::Unsupported);
        }

        let buffer = unsafe { self.spi.dma_state().empty_tx_buffer() };

        let bytes_to_write = address.width().div_ceil(8);
        // The address register is read in big-endian order,
        // we have to prepare the emulated write in the same way.
        let addr_bytes = address.value().to_be_bytes();
        let addr_bytes = &addr_bytes[4 - bytes_to_write..][..bytes_to_write];
        buffer.fill(addr_bytes);

        self.driver().setup_half_duplex(
            true,
            cmd,
            Address::None,
            false,
            dummy,
            bytes_to_write == 0,
            address.mode(),
        )?;

        let empty_rx_buffer = unsafe { self.dma_driver().empty_rx_buffer() };

        unsafe { self.start_transfer_dma(false, 0, bytes_to_write, empty_rx_buffer, buffer) }
    }

    fn cancel_transfer(&mut self) {
        let state = self.dma_driver().state;
        if state.tx_transfer_in_progress.get() || state.rx_transfer_in_progress.get() {
            self.dma_driver().abort_transfer();

            // We need to stop the DMA transfer, too.
            if state.tx_transfer_in_progress.get() {
                self.channel.tx.stop_transfer();
                state.tx_transfer_in_progress.set(false);
            }
            if state.rx_transfer_in_progress.get() {
                self.channel.rx.stop_transfer();
                state.rx_transfer_in_progress.set(false);
            }
        }
    }

    /// Cancel a transfer and wait until the engine has actually stopped.
    ///
    /// `cancel_transfer` only requests the stop, so returning before the
    /// engine halts would hand the buffer back while DMA may still write it.
    /// The wait is bounded: a `usr`-stuck fault must not spin in drop glue,
    /// and on exhaustion the DMA is reset instead.
    fn cancel_and_quiesce(&mut self) {
        self.cancel_transfer();
        CANCEL_QUIESCE_HITS.fetch_add(1, Ordering::Relaxed);

        let mut polls: u32 = 0;
        while !self.is_done() {
            polls += 1;
            if polls >= Self::CANCEL_QUIESCE_POLLS {
                self.dma_driver().reset_dma();
                CANCEL_QUIESCE_RESETS.fetch_add(1, Ordering::Relaxed);
                break;
            }
        }
        CANCEL_QUIESCE_MAX_POLLS.fetch_max(polls, Ordering::Relaxed);

        self.dma_driver().state.rx_transfer_in_progress.set(false);
        self.dma_driver().state.tx_transfer_in_progress.set(false);
        fence(Ordering::Acquire);
    }
}

#[instability::unstable]
impl<Dm> embassy_embedded_hal::SetConfig for SpiDma<'_, Dm>
where
    Dm: DriverMode,
{
    type Config = Config;
    type ConfigError = ConfigError;

    fn set_config(&mut self, config: &Self::Config) -> Result<(), Self::ConfigError> {
        self.apply_config(config)
    }
}

/// A structure representing a DMA transfer for SPI.
///
/// This structure holds references to the SPI instance, DMA buffers, and
/// transfer status.
#[instability::unstable]
pub struct SpiDmaTransfer<'d, Dm, Buf>
where
    Dm: DriverMode,
{
    spi_dma: ManuallyDrop<SpiDma<'d, Dm>>,
    dma_buf: ManuallyDrop<Buf>,
}

impl<Buf> SpiDmaTransfer<'_, Async, Buf> {
    /// Waits for the DMA transfer to complete asynchronously.
    ///
    /// This method awaits the completion of both RX and TX operations.
    #[instability::unstable]
    pub async fn wait_for_done(&mut self) {
        self.spi_dma.wait_for_idle_async().await;
    }
}

impl<'d, Dm, Buf> SpiDmaTransfer<'d, Dm, Buf>
where
    Dm: DriverMode,
{
    fn new(spi_dma: SpiDma<'d, Dm>, dma_buf: Buf) -> Self {
        Self {
            spi_dma: ManuallyDrop::new(spi_dma),
            dma_buf: ManuallyDrop::new(dma_buf),
        }
    }

    /// Checks if the transfer is complete.
    ///
    /// This method returns `true` if both RX and TX operations are done,
    /// and the SPI instance is no longer busy.
    pub fn is_done(&self) -> bool {
        self.spi_dma.is_done()
    }

    /// Waits for the DMA transfer to complete.
    ///
    /// This method blocks until the transfer is finished and returns the
    /// `SpiDma` instance and the associated buffer.
    #[instability::unstable]
    pub fn wait(mut self) -> (SpiDma<'d, Dm>, Buf) {
        self.spi_dma.wait_for_idle();
        let retval = unsafe {
            (
                ManuallyDrop::take(&mut self.spi_dma),
                ManuallyDrop::take(&mut self.dma_buf),
            )
        };
        core::mem::forget(self);
        retval
    }

    /// Cancels the DMA transfer.
    #[instability::unstable]
    pub fn cancel(&mut self) {
        if !self.spi_dma.is_done() {
            self.spi_dma.cancel_transfer();
        }
    }
}

impl<Dm, Buf> Drop for SpiDmaTransfer<'_, Dm, Buf>
where
    Dm: DriverMode,
{
    fn drop(&mut self) {
        if !self.is_done() {
            self.spi_dma.cancel_transfer();
            self.spi_dma.wait_for_idle();
        }

        unsafe {
            ManuallyDrop::drop(&mut self.spi_dma);
            ManuallyDrop::drop(&mut self.dma_buf);
        }
    }
}

impl<'d, Dm> SpiDma<'d, Dm>
where
    Dm: DriverMode,
{
    /// # Safety:
    ///
    /// The caller must ensure that the buffers are not accessed while the
    /// transfer is in progress. Moving the buffers is allowed.
    #[cfg_attr(place_spi_master_driver_in_ram, ram)]
    unsafe fn start_dma_write(
        &mut self,
        bytes_to_write: usize,
        buffer: &mut impl DmaTxBuffer,
    ) -> Result<(), Error> {
        let empty_rx_buffer = unsafe { self.dma_driver().empty_rx_buffer() };

        unsafe { self.start_dma_transfer(0, bytes_to_write, empty_rx_buffer, buffer) }
    }

    /// Configures the DMA buffers for the SPI instance.
    ///
    /// This method sets up both RX and TX buffers for DMA transfers.
    /// It returns an instance of `SpiDmaBus` that can be used for SPI
    /// communication.
    #[instability::unstable]
    pub fn with_buffers(self, dma_rx_buf: DmaRxBuf, dma_tx_buf: DmaTxBuf) -> SpiDmaBus<'d, Dm> {
        SpiDmaBus::new(self, dma_rx_buf, dma_tx_buf)
    }

    /// Perform a DMA write.
    ///
    /// This will return a [SpiDmaTransfer] owning the buffer and the
    /// SPI instance. The maximum amount of data to be sent is 32736
    /// bytes.
    #[allow(clippy::type_complexity)]
    #[cfg_attr(place_spi_master_driver_in_ram, ram)]
    #[instability::unstable]
    pub fn write<TX: DmaTxBuffer>(
        mut self,
        bytes_to_write: usize,
        mut buffer: TX,
    ) -> Result<SpiDmaTransfer<'d, Dm, TX>, (Error, Self, TX)> {
        self.wait_for_idle();
        if let Err(e) = self.driver().setup_full_duplex() {
            return Err((e, self, buffer));
        };
        match unsafe { self.start_dma_write(bytes_to_write, &mut buffer) } {
            Ok(_) => Ok(SpiDmaTransfer::new(self, buffer)),
            Err(e) => Err((e, self, buffer)),
        }
    }

    /// # Safety:
    ///
    /// The caller must ensure that the buffers are not accessed while the
    /// transfer is in progress. Moving the buffers is allowed.
    #[cfg_attr(place_spi_master_driver_in_ram, ram)]
    unsafe fn start_dma_read(
        &mut self,
        bytes_to_read: usize,
        buffer: &mut impl DmaRxBuffer,
    ) -> Result<(), Error> {
        let empty_tx_buffer = unsafe { self.dma_driver().empty_tx_buffer() };

        unsafe { self.start_dma_transfer(bytes_to_read, 0, buffer, empty_tx_buffer) }
    }

    /// Perform a DMA read.
    ///
    /// This will return a [SpiDmaTransfer] owning the buffer and
    /// the SPI instance. The maximum amount of data to be
    /// received is 32736 bytes.
    #[allow(clippy::type_complexity)]
    #[cfg_attr(place_spi_master_driver_in_ram, ram)]
    #[instability::unstable]
    pub fn read<RX: DmaRxBuffer>(
        mut self,
        bytes_to_read: usize,
        mut buffer: RX,
    ) -> Result<SpiDmaTransfer<'d, Dm, RX>, (Error, Self, RX)> {
        self.wait_for_idle();
        if let Err(e) = self.driver().setup_full_duplex() {
            return Err((e, self, buffer));
        };
        match unsafe { self.start_dma_read(bytes_to_read, &mut buffer) } {
            Ok(_) => Ok(SpiDmaTransfer::new(self, buffer)),
            Err(e) => Err((e, self, buffer)),
        }
    }

    /// # Safety:
    ///
    /// The caller must ensure that the buffers are not accessed while the
    /// transfer is in progress. Moving the buffers is allowed.
    #[cfg_attr(place_spi_master_driver_in_ram, ram)]
    unsafe fn start_dma_transfer(
        &mut self,
        bytes_to_read: usize,
        bytes_to_write: usize,
        rx_buffer: &mut impl DmaRxBuffer,
        tx_buffer: &mut impl DmaTxBuffer,
    ) -> Result<(), Error> {
        unsafe {
            self.start_transfer_dma(true, bytes_to_read, bytes_to_write, rx_buffer, tx_buffer)
        }
    }

    /// Perform a DMA transfer
    ///
    /// This will return a [SpiDmaTransfer] owning the buffers and
    /// the SPI instance. The maximum amount of data to be
    /// sent/received is 32736 bytes.
    #[allow(clippy::type_complexity)]
    #[cfg_attr(place_spi_master_driver_in_ram, ram)]
    #[instability::unstable]
    pub fn transfer<RX: DmaRxBuffer, TX: DmaTxBuffer>(
        mut self,
        bytes_to_read: usize,
        mut rx_buffer: RX,
        bytes_to_write: usize,
        mut tx_buffer: TX,
    ) -> Result<SpiDmaTransfer<'d, Dm, (RX, TX)>, (Error, Self, RX, TX)> {
        self.wait_for_idle();
        if let Err(e) = self.driver().setup_full_duplex() {
            return Err((e, self, rx_buffer, tx_buffer));
        };
        match unsafe {
            self.start_dma_transfer(
                bytes_to_read,
                bytes_to_write,
                &mut rx_buffer,
                &mut tx_buffer,
            )
        } {
            Ok(_) => Ok(SpiDmaTransfer::new(self, (rx_buffer, tx_buffer))),
            Err(e) => Err((e, self, rx_buffer, tx_buffer)),
        }
    }

    /// # Safety:
    ///
    /// The caller must ensure that the buffers are not accessed while the
    /// transfer is in progress. Moving the buffers is allowed.
    #[cfg_attr(place_spi_master_driver_in_ram, ram)]
    unsafe fn start_half_duplex_read(
        &mut self,
        data_mode: DataMode,
        cmd: Command,
        address: Address,
        dummy: u8,
        bytes_to_read: usize,
        buffer: &mut impl DmaRxBuffer,
    ) -> Result<(), Error> {
        self.driver().setup_half_duplex(
            false,
            cmd,
            address,
            false,
            dummy,
            bytes_to_read == 0,
            data_mode,
        )?;

        let empty_tx_buffer = unsafe { self.dma_driver().empty_tx_buffer() };

        unsafe { self.start_transfer_dma(false, bytes_to_read, 0, buffer, empty_tx_buffer) }
    }

    /// Perform a half-duplex read operation using DMA.
    #[allow(clippy::type_complexity)]
    #[cfg_attr(place_spi_master_driver_in_ram, ram)]
    #[instability::unstable]
    pub fn half_duplex_read<RX: DmaRxBuffer>(
        mut self,
        data_mode: DataMode,
        cmd: Command,
        address: Address,
        dummy: u8,
        bytes_to_read: usize,
        mut buffer: RX,
    ) -> Result<SpiDmaTransfer<'d, Dm, RX>, (Error, Self, RX)> {
        self.wait_for_idle();

        match unsafe {
            self.start_half_duplex_read(data_mode, cmd, address, dummy, bytes_to_read, &mut buffer)
        } {
            Ok(_) => Ok(SpiDmaTransfer::new(self, buffer)),
            Err(e) => Err((e, self, buffer)),
        }
    }

    /// # Safety:
    ///
    /// The caller must ensure that the buffers are not accessed while the
    /// transfer is in progress. Moving the buffers is allowed.
    #[cfg_attr(place_spi_master_driver_in_ram, ram)]
    unsafe fn start_half_duplex_write(
        &mut self,
        data_mode: DataMode,
        cmd: Command,
        address: Address,
        dummy: u8,
        bytes_to_write: usize,
        buffer: &mut impl DmaTxBuffer,
    ) -> Result<(), Error> {
        #[cfg(all(esp32, spi_address_workaround))]
        {
            // On the ESP32, if we don't have data, the address is always sent
            // on a single line, regardless of its data mode.
            if bytes_to_write == 0 && address.mode() != DataMode::SingleTwoDataLines {
                return unsafe { self.set_up_address_workaround(cmd, address, dummy) };
            }
        }

        self.driver().setup_half_duplex(
            true,
            cmd,
            address,
            false,
            dummy,
            bytes_to_write == 0,
            data_mode,
        )?;

        let empty_rx_buffer = unsafe { self.dma_driver().empty_rx_buffer() };

        unsafe { self.start_transfer_dma(false, 0, bytes_to_write, empty_rx_buffer, buffer) }
    }

    /// Perform a half-duplex write operation using DMA.
    #[allow(clippy::type_complexity)]
    #[cfg_attr(place_spi_master_driver_in_ram, ram)]
    #[instability::unstable]
    pub fn half_duplex_write<TX: DmaTxBuffer>(
        mut self,
        data_mode: DataMode,
        cmd: Command,
        address: Address,
        dummy: u8,
        bytes_to_write: usize,
        mut buffer: TX,
    ) -> Result<SpiDmaTransfer<'d, Dm, TX>, (Error, Self, TX)> {
        self.wait_for_idle();

        match unsafe {
            self.start_half_duplex_write(
                data_mode,
                cmd,
                address,
                dummy,
                bytes_to_write,
                &mut buffer,
            )
        } {
            Ok(_) => Ok(SpiDmaTransfer::new(self, buffer)),
            Err(e) => Err((e, self, buffer)),
        }
    }

    /// Change the bus configuration.
    ///
    /// # Errors
    ///
    /// If frequency passed in config exceeds
    #[cfg_attr(not(esp32h2), doc = " 80MHz")]
    #[cfg_attr(esp32h2, doc = " 48MHz")]
    /// or is below 70kHz,
    /// [`ConfigError::UnsupportedFrequency`] error will be returned.
    #[instability::unstable]
    pub fn apply_config(&mut self, config: &Config) -> Result<(), ConfigError> {
        self.driver().apply_config(config)
    }
}

/// A DMA-capable SPI bus.
///
/// This structure is responsible for managing SPI transfers using DMA
/// buffers.
#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[instability::unstable]
pub struct SpiDmaBus<'d, Dm>
where
    Dm: DriverMode,
{
    spi_dma: SpiDma<'d, Dm>,
    rx_buf: DmaRxBuf,
    tx_buf: DmaTxBuf,
}

impl<Dm> crate::private::Sealed for SpiDmaBus<'_, Dm> where Dm: DriverMode {}

impl<'d> SpiDmaBus<'d, Blocking> {
    /// Converts the SPI instance into async mode.
    #[instability::unstable]
    pub fn into_async(self) -> SpiDmaBus<'d, Async> {
        SpiDmaBus {
            spi_dma: self.spi_dma.into_async(),
            rx_buf: self.rx_buf,
            tx_buf: self.tx_buf,
        }
    }

    /// Listen for the given interrupts
    #[instability::unstable]
    pub fn listen(&mut self, interrupts: impl Into<EnumSet<SpiInterrupt>>) {
        self.spi_dma.listen(interrupts.into());
    }

    /// Unlisten the given interrupts
    #[instability::unstable]
    pub fn unlisten(&mut self, interrupts: impl Into<EnumSet<SpiInterrupt>>) {
        self.spi_dma.unlisten(interrupts.into());
    }

    /// Gets asserted interrupts
    #[instability::unstable]
    pub fn interrupts(&mut self) -> EnumSet<SpiInterrupt> {
        self.spi_dma.interrupts()
    }

    /// Resets asserted interrupts
    #[instability::unstable]
    pub fn clear_interrupts(&mut self, interrupts: impl Into<EnumSet<SpiInterrupt>>) {
        self.spi_dma.clear_interrupts(interrupts.into());
    }
}

impl<'d> SpiDmaBus<'d, Async> {
    /// Converts the SPI instance into async mode.
    #[instability::unstable]
    pub fn into_blocking(self) -> SpiDmaBus<'d, Blocking> {
        SpiDmaBus {
            spi_dma: self.spi_dma.into_blocking(),
            rx_buf: self.rx_buf,
            tx_buf: self.tx_buf,
        }
    }

    /// Fill the given buffer with data from the bus.
    #[instability::unstable]
    pub async fn read_async(&mut self, words: &mut [u8]) -> Result<(), Error> {
        self.spi_dma.wait_for_idle_async().await;
        self.spi_dma.driver().setup_full_duplex()?;
        let chunk_size = self.rx_buf.capacity();

        let empty_tx_buffer = unsafe { self.spi_dma.dma_driver().empty_tx_buffer() };

        for chunk in words.chunks_mut(chunk_size) {
            let mut spi = DropGuard::new(&mut self.spi_dma, |spi| spi.cancel_and_quiesce());

            unsafe { spi.start_dma_transfer(chunk.len(), 0, &mut self.rx_buf, empty_tx_buffer)? };

            spi.wait_for_idle_async().await;

            chunk.copy_from_slice(&self.rx_buf.as_slice()[..chunk.len()]);

            spi.defuse();
        }

        Ok(())
    }

    /// Transmit the given buffer to the bus.
    ///
    /// Tries a zero-copy DMA path when the slice lives in DRAM and the address
    /// is suitably aligned: descriptors are built on the stack, pointing at the
    /// caller's buffer directly, and no copy into `tx_buf` is performed. Falls
    /// back to the chunked-copy path through `tx_buf` when zero-copy isn't
    /// applicable (e.g. flash-resident `&str` literals).
    ///
    /// ESP32 (LX6 / PDMA) unaligned handling: SPI PDMA TX wedges if the
    /// descriptor byte count is not a 4-byte multiple. When the slice is
    /// unaligned, a single chained descriptor list is built: the bulk
    /// (4-aligned prefix, possibly zero) followed by a 4-byte stack-padded
    /// tail descriptor. The DMA streams `bulk + pad` as one continuous burst
    /// — no clock gap mid-transfer — and SPI's `MOSI_DBITLEN` is programmed
    /// to the exact bit count, so the 1-3 zero-pad bytes are read by DMA but
    /// never clocked to the slave.
    #[instability::unstable]
    pub async fn write_async(&mut self, words: &[u8]) -> Result<(), Error> {
        if words.is_empty() {
            return Ok(());
        }

        self.spi_dma.wait_for_idle_async().await;
        self.spi_dma.driver().setup_full_duplex()?;

        let empty_rx_buffer = unsafe { self.spi_dma.dma_driver().empty_rx_buffer() };

        // Stack descriptor count: enough to chain MAX_DMA_SIZE bytes at the
        // worst-case 4-byte alignment (chunk = 4096 - 4 = 4092). +2 for slop,
        // +1 to leave room for an appended tail-pad descriptor on ESP32.
        const ZC_DESC_COUNT: usize = MAX_DMA_SIZE.div_ceil(4092) + 3;

        // ESP32-only: caller-owned 4-byte pad buffer for the unaligned tail.
        // Must be word-aligned (ESP32 PDMA buffer-address requirement); use
        // `[u32; 1]` to force 4-byte alignment, then view as `[u8; 4]` via
        // cast. Lives for the whole function (and any await within), so the
        // DMA descriptor chain can reference it safely.
        #[cfg(esp32)]
        let mut pad_word = [0u32; 1];
        #[cfg(esp32)]
        // SAFETY: u32 has the same size and stricter alignment than [u8; 4].
        let pad: &mut [u8; 4] = unsafe { &mut *pad_word.as_mut_ptr().cast::<[u8; 4]>() };

        // Zero-copy path: caller's buffer is in DRAM. The unaligned tail (if
        // any) gets chained as an extra descriptor pointing at `pad`.
        if is_slice_in_dram(words) {
            let mut offset = 0;
            while offset < words.len() {
                let remaining = &words[offset..];
                let mut descriptors = [DmaDescriptor::EMPTY; ZC_DESC_COUNT];

                #[cfg(esp32)]
                let prep = unsafe {
                    prepare_for_tx_with_pad(
                        &mut descriptors,
                        NonNull::from(remaining),
                        pad,
                        1,
                    )
                };
                #[cfg(not(esp32))]
                let prep = unsafe {
                    prepare_for_tx(&mut descriptors, NonNull::from(remaining), 1)
                };

                match prep {
                    Ok((mut tx_buf, transferred)) => {
                        let mut spi = DropGuard::new(
                            &mut self.spi_dma,
                            |spi| spi.cancel_and_quiesce(),
                        );
                        unsafe {
                            spi.start_dma_transfer(
                                0,
                                transferred,
                                empty_rx_buffer,
                                &mut tx_buf,
                            )?;
                        }
                        spi.wait_for_idle_async().await;
                        spi.defuse();
                        offset += transferred;
                    }
                    Err(_) => break, // alignment mismatch — fall back below
                }
            }
            if offset == words.len() {
                return Ok(());
            }
            // Partial zero-copy + copy fallback for the remainder.
            return self.write_async_copy(&words[offset..], empty_rx_buffer).await;
        }

        // Copy path (full): for non-DRAM slices (e.g. flash literals).
        self.write_async_copy(words, empty_rx_buffer).await
    }

    async fn write_async_copy(
        &mut self,
        words: &[u8],
        empty_rx_buffer: &'static mut DmaRxBuf,
    ) -> Result<(), Error> {
        let mut spi = DropGuard::new(&mut self.spi_dma, |spi| spi.cancel_and_quiesce());
        let chunk_size = self.tx_buf.capacity();
        for chunk in words.chunks(chunk_size) {
            self.tx_buf.as_mut_slice()[..chunk.len()].copy_from_slice(chunk);
            unsafe { spi.start_dma_transfer(0, chunk.len(), empty_rx_buffer, &mut self.tx_buf)? };
            spi.wait_for_idle_async().await;
        }
        spi.defuse();
        Ok(())
    }

    /// Transfer by writing out a buffer and reading the response from
    /// the bus into another buffer.
    #[instability::unstable]
    pub async fn transfer_async(&mut self, read: &mut [u8], write: &[u8]) -> Result<(), Error> {
        self.spi_dma.wait_for_idle_async().await;
        self.spi_dma.driver().setup_full_duplex()?;

        let mut spi = DropGuard::new(&mut self.spi_dma, |spi| spi.cancel_and_quiesce());
        let chunk_size = min(self.tx_buf.capacity(), self.rx_buf.capacity());

        let common_length = min(read.len(), write.len());
        let (read_common, read_remainder) = read.split_at_mut(common_length);
        let (write_common, write_remainder) = write.split_at(common_length);

        for (read_chunk, write_chunk) in read_common
            .chunks_mut(chunk_size)
            .zip(write_common.chunks(chunk_size))
        {
            self.tx_buf.as_mut_slice()[..write_chunk.len()].copy_from_slice(write_chunk);

            unsafe {
                spi.start_dma_transfer(
                    read_chunk.len(),
                    write_chunk.len(),
                    &mut self.rx_buf,
                    &mut self.tx_buf,
                )?;
            }
            spi.wait_for_idle_async().await;

            read_chunk.copy_from_slice(&self.rx_buf.as_slice()[..read_chunk.len()]);
        }

        spi.defuse();

        if !read_remainder.is_empty() {
            self.read_async(read_remainder).await
        } else if !write_remainder.is_empty() {
            self.write_async(write_remainder).await
        } else {
            Ok(())
        }
    }

    /// Transfer by writing out a buffer and reading the response from
    /// the bus into the same buffer.
    #[instability::unstable]
    pub async fn transfer_in_place_async(&mut self, words: &mut [u8]) -> Result<(), Error> {
        self.spi_dma.wait_for_idle_async().await;
        self.spi_dma.driver().setup_full_duplex()?;

        let mut spi = DropGuard::new(&mut self.spi_dma, |spi| spi.cancel_and_quiesce());
        for chunk in words.chunks_mut(self.tx_buf.capacity()) {
            self.tx_buf.as_mut_slice()[..chunk.len()].copy_from_slice(chunk);

            unsafe {
                spi.start_dma_transfer(
                    chunk.len(),
                    chunk.len(),
                    &mut self.rx_buf,
                    &mut self.tx_buf,
                )?;
            }
            spi.wait_for_idle_async().await;
            chunk.copy_from_slice(&self.rx_buf.as_slice()[..chunk.len()]);
        }

        spi.defuse();

        Ok(())
    }
}

impl<'d, Dm> SpiDmaBus<'d, Dm>
where
    Dm: DriverMode,
{
    /// Creates a new `SpiDmaBus` with the specified SPI instance and DMA
    /// buffers.
    pub fn new(spi_dma: SpiDma<'d, Dm>, rx_buf: DmaRxBuf, tx_buf: DmaTxBuf) -> Self {
        Self {
            spi_dma,
            rx_buf,
            tx_buf,
        }
    }

    /// Splits [SpiDmaBus] back into [SpiDma], [DmaRxBuf] and [DmaTxBuf].
    #[instability::unstable]
    pub fn split(mut self) -> (SpiDma<'d, Dm>, DmaRxBuf, DmaTxBuf) {
        self.wait_for_idle();
        (self.spi_dma, self.rx_buf, self.tx_buf)
    }

    fn wait_for_idle(&mut self) {
        self.spi_dma.wait_for_idle();
    }

    /// Change the bus configuration.
    ///
    /// # Errors
    ///
    /// If frequency passed in config exceeds
    #[cfg_attr(not(esp32h2), doc = " 80MHz")]
    #[cfg_attr(esp32h2, doc = " 48MHz")]
    /// or is below 70kHz,
    /// [`ConfigError::UnsupportedFrequency`] error will be returned.
    #[instability::unstable]
    pub fn apply_config(&mut self, config: &Config) -> Result<(), ConfigError> {
        self.spi_dma.apply_config(config)
    }

    /// Reads data from the SPI bus using DMA.
    #[instability::unstable]
    pub fn read(&mut self, words: &mut [u8]) -> Result<(), Error> {
        self.wait_for_idle();
        self.spi_dma.driver().setup_full_duplex()?;

        let empty_tx_buffer = unsafe { self.spi_dma.dma_driver().empty_tx_buffer() };

        for chunk in words.chunks_mut(self.rx_buf.capacity()) {
            unsafe {
                self.spi_dma.start_dma_transfer(
                    chunk.len(),
                    0,
                    &mut self.rx_buf,
                    empty_tx_buffer,
                )?;
            }

            self.wait_for_idle();
            chunk.copy_from_slice(&self.rx_buf.as_slice()[..chunk.len()]);
        }

        Ok(())
    }

    /// Writes data to the SPI bus using DMA.
    #[instability::unstable]
    pub fn write(&mut self, words: &[u8]) -> Result<(), Error> {
        self.wait_for_idle();
        self.spi_dma.driver().setup_full_duplex()?;
        let empty_rx_buffer = unsafe { self.spi_dma.dma_driver().empty_rx_buffer() };

        for chunk in words.chunks(self.tx_buf.capacity()) {
            self.tx_buf.as_mut_slice()[..chunk.len()].copy_from_slice(chunk);

            unsafe {
                self.spi_dma.start_dma_transfer(
                    0,
                    chunk.len(),
                    empty_rx_buffer,
                    &mut self.tx_buf,
                )?;
            }

            self.wait_for_idle();
        }

        Ok(())
    }

    /// Transfers data to and from the SPI bus simultaneously using DMA.
    #[instability::unstable]
    pub fn transfer(&mut self, read: &mut [u8], write: &[u8]) -> Result<(), Error> {
        self.wait_for_idle();
        self.spi_dma.driver().setup_full_duplex()?;
        let chunk_size = min(self.tx_buf.capacity(), self.rx_buf.capacity());

        let common_length = min(read.len(), write.len());
        let (read_common, read_remainder) = read.split_at_mut(common_length);
        let (write_common, write_remainder) = write.split_at(common_length);

        for (read_chunk, write_chunk) in read_common
            .chunks_mut(chunk_size)
            .zip(write_common.chunks(chunk_size))
        {
            self.tx_buf.as_mut_slice()[..write_chunk.len()].copy_from_slice(write_chunk);

            unsafe {
                self.spi_dma.start_dma_transfer(
                    read_chunk.len(),
                    write_chunk.len(),
                    &mut self.rx_buf,
                    &mut self.tx_buf,
                )?;
            }
            self.wait_for_idle();

            read_chunk.copy_from_slice(&self.rx_buf.as_slice()[..read_chunk.len()]);
        }

        if !read_remainder.is_empty() {
            self.read(read_remainder)
        } else if !write_remainder.is_empty() {
            self.write(write_remainder)
        } else {
            Ok(())
        }
    }

    /// Transfers data in place on the SPI bus using DMA.
    #[instability::unstable]
    pub fn transfer_in_place(&mut self, words: &mut [u8]) -> Result<(), Error> {
        self.wait_for_idle();
        self.spi_dma.driver().setup_full_duplex()?;
        let chunk_size = min(self.tx_buf.capacity(), self.rx_buf.capacity());

        for chunk in words.chunks_mut(chunk_size) {
            self.tx_buf.as_mut_slice()[..chunk.len()].copy_from_slice(chunk);

            unsafe {
                self.spi_dma.start_dma_transfer(
                    chunk.len(),
                    chunk.len(),
                    &mut self.rx_buf,
                    &mut self.tx_buf,
                )?;
            }
            self.wait_for_idle();
            chunk.copy_from_slice(&self.rx_buf.as_slice()[..chunk.len()]);
        }

        Ok(())
    }

    /// Half-duplex read.
    #[instability::unstable]
    pub fn half_duplex_read(
        &mut self,
        data_mode: DataMode,
        cmd: Command,
        address: Address,
        dummy: u8,
        buffer: &mut [u8],
    ) -> Result<(), Error> {
        if buffer.len() > self.rx_buf.capacity() {
            return Err(Error::from(DmaError::Overflow));
        }
        self.wait_for_idle();

        unsafe {
            self.spi_dma.start_half_duplex_read(
                data_mode,
                cmd,
                address,
                dummy,
                buffer.len(),
                &mut self.rx_buf,
            )?;
        }

        self.wait_for_idle();

        buffer.copy_from_slice(&self.rx_buf.as_slice()[..buffer.len()]);

        Ok(())
    }

    /// Half-duplex write.
    #[instability::unstable]
    pub fn half_duplex_write(
        &mut self,
        data_mode: DataMode,
        cmd: Command,
        address: Address,
        dummy: u8,
        buffer: &[u8],
    ) -> Result<(), Error> {
        if buffer.len() > self.tx_buf.capacity() {
            return Err(Error::from(DmaError::Overflow));
        }
        self.wait_for_idle();
        self.tx_buf.as_mut_slice()[..buffer.len()].copy_from_slice(buffer);

        unsafe {
            self.spi_dma.start_half_duplex_write(
                data_mode,
                cmd,
                address,
                dummy,
                buffer.len(),
                &mut self.tx_buf,
            )?;
        }

        self.wait_for_idle();

        Ok(())
    }
}

#[instability::unstable]
impl crate::interrupt::InterruptConfigurable for SpiDmaBus<'_, Blocking> {
    /// Sets the interrupt handler
    ///
    /// Interrupts are not enabled at the peripheral level here.
    fn set_interrupt_handler(&mut self, handler: InterruptHandler) {
        self.spi_dma.set_interrupt_handler(handler);
    }
}

#[instability::unstable]
impl<Dm> embassy_embedded_hal::SetConfig for SpiDmaBus<'_, Dm>
where
    Dm: DriverMode,
{
    type Config = Config;
    type ConfigError = ConfigError;

    fn set_config(&mut self, config: &Self::Config) -> Result<(), Self::ConfigError> {
        self.apply_config(config)
    }
}

pub(super) struct DmaDriver {
    driver: Driver,
    dma_peripheral: crate::dma::DmaPeripheral,
    state: &'static DmaState,
}

impl DmaDriver {
    unsafe fn empty_rx_buffer(&self) -> &'static mut DmaRxBuf {
        unsafe { self.state.empty_rx_buffer() }
    }

    unsafe fn empty_tx_buffer(&self) -> &'static mut DmaTxBuf {
        unsafe { self.state.empty_tx_buffer() }
    }

    fn abort_transfer(&self) {
        // The SPI peripheral is controlling how much data we transfer, so let's
        // update its counter.
        // 0 doesn't take effect on ESP32 and cuts the currently transmitted byte
        // immediately.
        // 1 seems to stop after transmitting the current byte which is somewhat less
        // impolite.
        self.driver.configure_datalen(1, 1);
        self.driver.update();
    }

    fn regs(&self) -> &RegisterBlock {
        self.driver.regs()
    }

    #[cfg_attr(place_spi_master_driver_in_ram, ram)]
    unsafe fn start_transfer_dma<Dm: DriverMode>(
        &self,
        _full_duplex: bool,
        rx_len: usize,
        tx_len: usize,
        rx_buffer: &mut impl DmaRxBuffer,
        tx_buffer: &mut impl DmaTxBuffer,
        channel: &mut Channel<Dm, PeripheralDmaChannel<AnySpi<'_>>>,
    ) -> Result<(), Error> {
        #[cfg(esp32s2)]
        {
            // without this a transfer after a write will fail
            self.regs().dma_out_link().write(|w| unsafe { w.bits(0) });
            self.regs().dma_in_link().write(|w| unsafe { w.bits(0) });
        }

        self.driver.configure_datalen(rx_len, tx_len);

        // enable the MISO and MOSI if needed
        self.regs()
            .user()
            .modify(|_, w| w.usr_miso().bit(rx_len > 0).usr_mosi().bit(tx_len > 0));

        self.enable_dma();

        if rx_len > 0 {
            unsafe {
                channel
                    .rx
                    .prepare_transfer(self.dma_peripheral, rx_buffer)
                    .and_then(|_| channel.rx.start_transfer())?;
            }
        } else {
            #[cfg(esp32)]
            {
                // see https://github.com/espressif/esp-idf/commit/366e4397e9dae9d93fe69ea9d389b5743295886f
                // see https://github.com/espressif/esp-idf/commit/0c3653b1fd7151001143451d4aa95dbf15ee8506
                if _full_duplex {
                    self.regs()
                        .dma_in_link()
                        .modify(|_, w| unsafe { w.inlink_addr().bits(0) });
                    self.regs()
                        .dma_in_link()
                        .modify(|_, w| w.inlink_start().set_bit());
                }
            }
        }
        if tx_len > 0 {
            unsafe {
                channel
                    .tx
                    .prepare_transfer(self.dma_peripheral, tx_buffer)
                    .and_then(|_| channel.tx.start_transfer())?;
            }
        }

        #[cfg(dma_kind = "gdma")]
        self.reset_dma();

        self.driver.start_operation();

        Ok(())
    }

    fn enable_dma(&self) {
        #[cfg(dma_kind = "gdma")]
        // for non GDMA this is done in `assign_tx_device` / `assign_rx_device`
        self.regs().dma_conf().modify(|_, w| {
            w.dma_tx_ena().set_bit();
            w.dma_rx_ena().set_bit()
        });

        #[cfg(dma_kind = "pdma")]
        self.reset_dma();
    }

    fn reset_dma(&self) {
        #[cfg(dma_kind = "pdma")]
        self.regs().dma_conf().toggle(|w, bit| {
            w.out_rst().bit(bit);
            w.in_rst().bit(bit);
            w.ahbm_fifo_rst().bit(bit);
            w.ahbm_rst().bit(bit)
        });

        #[cfg(dma_kind = "gdma")]
        self.regs().dma_conf().toggle(|w, bit| {
            w.rx_afifo_rst().bit(bit);
            w.buf_afifo_rst().bit(bit);
            w.dma_afifo_rst().bit(bit)
        });

        self.clear_dma_interrupts();
    }

    #[cfg(dma_kind = "gdma")]
    fn clear_dma_interrupts(&self) {
        self.regs().dma_int_clr().write(|w| {
            w.dma_infifo_full_err().clear_bit_by_one();
            w.dma_outfifo_empty_err().clear_bit_by_one();
            w.trans_done().clear_bit_by_one();
            w.mst_rx_afifo_wfull_err().clear_bit_by_one();
            w.mst_tx_afifo_rempty_err().clear_bit_by_one()
        });
    }

    #[cfg(dma_kind = "pdma")]
    fn clear_dma_interrupts(&self) {
        self.regs().dma_int_clr().write(|w| {
            w.inlink_dscr_empty().clear_bit_by_one();
            w.outlink_dscr_error().clear_bit_by_one();
            w.inlink_dscr_error().clear_bit_by_one();
            w.in_done().clear_bit_by_one();
            w.in_err_eof().clear_bit_by_one();
            w.in_suc_eof().clear_bit_by_one();
            w.out_done().clear_bit_by_one();
            w.out_eof().clear_bit_by_one();
            w.out_total_eof().clear_bit_by_one()
        });
    }
}

impl<'d> DmaEligible for AnySpi<'d> {
    #[cfg(dma_kind = "gdma")]
    type Dma = crate::dma::AnyGdmaChannel<'d>;
    #[cfg(dma_kind = "pdma")]
    type Dma = crate::dma::AnySpiDmaChannel<'d>;

    fn dma_peripheral(&self) -> crate::dma::DmaPeripheral {
        let (info, _state) = self.dma_parts();
        info.dma_peripheral
    }
}

#[instability::unstable]
impl embedded_hal_async::spi::SpiBus for SpiDmaBus<'_, Async> {
    async fn read(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
        self.read_async(words).await
    }

    async fn write(&mut self, words: &[u8]) -> Result<(), Self::Error> {
        self.write_async(words).await
    }

    async fn transfer(&mut self, read: &mut [u8], write: &[u8]) -> Result<(), Self::Error> {
        self.transfer_async(read, write).await
    }

    async fn transfer_in_place(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
        self.transfer_in_place_async(words).await
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        // All operations currently flush so this is no-op.
        Ok(())
    }
}

#[instability::unstable]
impl<Dm> ErrorType for SpiDmaBus<'_, Dm>
where
    Dm: DriverMode,
{
    type Error = Error;
}

#[instability::unstable]
impl<Dm> SpiBus for SpiDmaBus<'_, Dm>
where
    Dm: DriverMode,
{
    fn read(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
        self.read(words)
    }

    fn write(&mut self, words: &[u8]) -> Result<(), Self::Error> {
        self.write(words)
    }

    fn transfer(&mut self, read: &mut [u8], write: &[u8]) -> Result<(), Self::Error> {
        self.transfer(read, write)
    }

    fn transfer_in_place(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
        self.transfer_in_place(words)
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        // All operations currently flush so this is no-op.
        Ok(())
    }
}

struct DmaInfo {
    dma_peripheral: crate::dma::DmaPeripheral,
}
struct DmaState {
    tx_transfer_in_progress: Cell<bool>,
    rx_transfer_in_progress: Cell<bool>,

    empty_rx_buffer: UnsafeCell<MaybeUninit<DmaRxBuf>>,
    empty_tx_buffer: UnsafeCell<MaybeUninit<DmaTxBuf>>,
}

impl DmaState {
    // Syntactic helper to get a mutable reference to the "empty" RX DMA buffer.
    //
    // # Safety
    //
    // The caller must ensure that Rust's aliasing rules are upheld.
    #[allow(
        clippy::mut_from_ref,
        reason = "Safety requirements ensure this is okay"
    )]
    unsafe fn empty_rx_buffer(&self) -> &mut DmaRxBuf {
        unsafe { (&mut *self.empty_rx_buffer.get()).assume_init_mut() }
    }

    // Syntactic helper to get a mutable reference to the "empty" TX DMA buffer.
    //
    // # Safety
    //
    // The caller must ensure that Rust's aliasing rules are upheld.
    #[allow(
        clippy::mut_from_ref,
        reason = "Safety requirements ensure this is okay"
    )]
    unsafe fn empty_tx_buffer(&self) -> &mut DmaTxBuf {
        unsafe { (&mut *self.empty_tx_buffer.get()).assume_init_mut() }
    }
}

// SAFETY: State belongs to the currently constructed driver instance. As such, it'll not be
// accessed concurrently in multiple threads.
unsafe impl Sync for DmaState {}

for_each_spi_master!(
    (all $( ($peri:ident, $sys:ident, $sclk:ident $_cs:tt $_sio:tt $(, $is_qspi:tt)?)),* ) => {
        impl AnySpi<'_> {
            #[inline(always)]
            fn dma_parts(&self) -> (&'static DmaInfo, &'static DmaState) {
                match &self.0 {
                    $(
                        super::any::Inner::$sys(_spi) => {
                            static DMA_INFO: DmaInfo = DmaInfo {
                                dma_peripheral: crate::dma::DmaPeripheral::$sys,
                            };

                            static DMA_STATE: DmaState = DmaState {
                                tx_transfer_in_progress: Cell::new(false),
                                rx_transfer_in_progress: Cell::new(false),

                                empty_rx_buffer: UnsafeCell::new(MaybeUninit::uninit()),
                                empty_tx_buffer: UnsafeCell::new(MaybeUninit::uninit()),
                            };

                            (&DMA_INFO, &DMA_STATE)
                        }
                    )*
                }
            }

            #[inline(always)]
            fn dma_state(&self) -> &'static DmaState {
                let (_, state) = self.dma_parts();
                state
            }

            #[inline(always)]
            fn dma_info(&self) -> &'static DmaInfo {
                let (info, _) = self.dma_parts();
                info
            }
        }
    };
);

impl SpiWrapper<'_> {
    fn dma_state(&self) -> &'static DmaState {
        self.spi.dma_state()
    }

    #[inline(always)]
    fn dma_peripheral(&self) -> crate::dma::DmaPeripheral {
        self.spi.dma_peripheral()
    }
}
