//! Command-line entry parsing. Kept free of GTK so it can be unit-tested.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launch {
    /// Show the main settings window.
    Main,
    /// Open the quick-fill panel (hotkey path).
    QuickFill,
    /// Started by D-Bus activation / autostart: stay resident, no window.
    Service,
    /// Open the first-run setup wizard directly.
    Setup,
}

/// `args` includes argv[0].
pub fn parse_launch(args: &[String]) -> Launch {
    let mut launch = Launch::Main;
    for a in args.iter().skip(1) {
        match a.as_str() {
            "--quick-fill" => return Launch::QuickFill,
            "--setup" => return Launch::Setup,
            "--gapplication-service" => launch = Launch::Service,
            _ => {}
        }
    }
    launch
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_args_is_main() {
        assert_eq!(parse_launch(&v(&["immurok-gui"])), Launch::Main);
    }

    #[test]
    fn quick_fill_flag() {
        assert_eq!(parse_launch(&v(&["immurok-gui", "--quick-fill"])), Launch::QuickFill);
    }

    #[test]
    fn setup_flag() {
        assert_eq!(parse_launch(&v(&["immurok-gui", "--setup"])), Launch::Setup);
    }

    #[test]
    fn service_flag() {
        assert_eq!(parse_launch(&v(&["immurok-gui", "--gapplication-service"])), Launch::Service);
    }

    #[test]
    fn quick_fill_wins_over_service() {
        assert_eq!(
            parse_launch(&v(&["immurok-gui", "--gapplication-service", "--quick-fill"])),
            Launch::QuickFill
        );
    }
}
