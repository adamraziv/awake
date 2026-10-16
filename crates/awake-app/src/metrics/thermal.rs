//! Thermal pressure from `NSProcessInfo.thermalState`. Spec section 7.

use objc2_foundation::{NSProcessInfo, NSProcessInfoThermalState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThermalState {
    Nominal,
    Fair,
    Serious,
    Critical,
}

impl ThermalState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Nominal => "nominal",
            Self::Fair => "fair",
            Self::Serious => "serious",
            Self::Critical => "critical",
        }
    }

    fn from_ns(state: NSProcessInfoThermalState) -> Option<Self> {
        match state {
            NSProcessInfoThermalState::Nominal => Some(Self::Nominal),
            NSProcessInfoThermalState::Fair => Some(Self::Fair),
            NSProcessInfoThermalState::Serious => Some(Self::Serious),
            NSProcessInfoThermalState::Critical => Some(Self::Critical),
            _ => None,
        }
    }
}

/// `None` only if a future macOS adds a fifth state.
pub fn read() -> Option<ThermalState> {
    ThermalState::from_ns(NSProcessInfo::processInfo().thermalState())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_the_four_states_and_rejects_unknown() {
        assert_eq!(ThermalState::from_ns(NSProcessInfoThermalState::Nominal), Some(ThermalState::Nominal));
        assert_eq!(ThermalState::from_ns(NSProcessInfoThermalState::Fair), Some(ThermalState::Fair));
        assert_eq!(ThermalState::from_ns(NSProcessInfoThermalState::Serious), Some(ThermalState::Serious));
        assert_eq!(ThermalState::from_ns(NSProcessInfoThermalState::Critical), Some(ThermalState::Critical));
        assert_eq!(ThermalState::from_ns(NSProcessInfoThermalState(99)), None);
        assert_eq!(ThermalState::Serious.as_str(), "serious");
    }

    #[test]
    fn host_reports_a_state() {
        assert!(read().is_some());
    }
}
