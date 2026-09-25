const PERCENT: f64 = 100.0;

/// What a trade pays and gets back in USD, as priced by the quote's provider.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UsdValue {
    paid: f64,
    received: f64,
}

impl UsdValue {
    pub(crate) fn new(paid: f64, received: f64) -> Option<Self> {
        (paid.is_finite() && paid > 0.0 && received.is_finite() && received >= 0.0)
            .then_some(Self { paid, received })
    }

    /// Providers may omit either side; both are needed to show a loss.
    pub(crate) fn from_reported(paid: Option<f64>, received: Option<f64>) -> Option<Self> {
        Self::new(paid?, received?)
    }

    pub fn paid(&self) -> f64 {
        self.paid
    }

    pub fn received(&self) -> f64 {
        self.received
    }

    /// Paid value not received back: fees, price impact and the provider's price error together.
    /// Negative when the provider values the output above the input.
    pub fn loss_percent(&self) -> f64 {
        (self.paid - self.received) / self.paid * PERCENT
    }
}
