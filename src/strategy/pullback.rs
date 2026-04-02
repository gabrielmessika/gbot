//! Phase 7.5 — Entry timing: vrai pullback + flow confirmation.
//!
//! Séquence complète d'entrée :
//!   1. direction_score > seuil + N confirmations consécutives (main.rs)
//!   2. micro-move détecté : prix bouge ≥ min_move_bps dans la direction du signal
//!   3. pullback : retrace ≥ pullback_retrace_pct % du micro-move
//!   4. confirmation OFI : ofi_10s repasse > ofi_confirm_threshold dans la direction
//!   5. Émission du signal d'entrée
//!
//! Abandon si :
//!   - timeout (expires_at dépassé)
//!   - pullback > 100% du micro-move (renversement, pas un pullback)
//!   - signal opposé détecté
//!
//! Le PullbackTracker est instancié une fois dans main.rs et mis à jour à chaque
//! BookUpdate via `update()`. Quand un setup se complète, `update()` retourne
//! `Some(ReadyEntry { ... })` que main.rs peut convertir en `Intent::PlacePassiveEntry`.

use std::collections::HashMap;

use rust_decimal::Decimal;
use tracing::{debug, info};

use crate::strategy::signal::Direction;

/// Paramètres de la logique de pullback, issus de StrategySettings.
#[derive(Debug, Clone)]
pub struct PullbackSettings {
    /// Micro-move minimum pour activer l'attente de pullback (bps).
    /// En-dessous, on considère qu'il n'y a pas de momentum directionnel mesurable.
    pub min_move_bps: f64,
    /// Retrace minimum exprimé en fraction du micro-move (ex: 0.35 = 35%).
    pub retrace_pct: f64,
    /// Délai maximum pour que le micro-move se produise (phase WaitingMove), ms.
    pub wait_move_ms: i64,
    /// Délai maximum pour que le retrace se produise après micro-move confirmé (phase WaitingPullback), ms.
    pub wait_retrace_ms: i64,
    /// Seuil OFI 10s pour confirmer la reprise directionnelle post-pullback.
    /// Pour un Long : ofi_10s > threshold. Pour un Short : ofi_10s < -threshold.
    pub ofi_confirm_threshold: f64,
}

/// Un setup prêt à être exécuté, retourné par `PullbackTracker::update()`.
#[derive(Debug, Clone)]
pub struct ReadyEntry {
    pub coin: String,
    pub direction: Direction,
    /// Prix d'entrée passif (mid au moment du pullback, ajusté au bid/ask).
    pub entry_mid: f64,
    /// Distance SL en pourcentage (relative), héritée du signal initial.
    pub sl_pct: f64,
    /// Distance TP en pourcentage (relative), héritée du signal initial.
    pub tp_pct: f64,
    pub size: Decimal,
    pub max_wait_s: u64,
    /// Score directionnel au moment du signal (pour journalisation).
    pub dir_score: f64,
}

/// Phase de la state machine par coin.
#[derive(Debug, Clone)]
enum Phase {
    /// En attente d'un micro-move directionnel.
    WaitingMove {
        direction: Direction,
        /// Mid au moment de la confirmation directionnelle.
        initial_mid: f64,
        /// Extrême atteint dans la direction (high pour Long, low pour Short).
        extreme_mid: f64,
        expires_at: i64,
        sl_pct: f64,
        tp_pct: f64,
        size: Decimal,
        max_wait_s: u64,
        dir_score: f64,
    },
    /// Micro-move confirmé, on attend le pullback + confirmation OFI.
    WaitingPullback {
        direction: Direction,
        initial_mid: f64,
        extreme_mid: f64,
        expires_at: i64,
        sl_pct: f64,
        tp_pct: f64,
        size: Decimal,
        max_wait_s: u64,
        dir_score: f64,
    },
}

/// Raison d'abandon d'un setup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbandonReason {
    Timeout,
    Reversal,
    OppositeSignal,
}

/// Résultat d'un appel à `update()`.
#[derive(Debug)]
pub enum UpdateResult {
    /// Pas encore prêt — attente en cours.
    Waiting,
    /// Setup complété — entrée prête à être exécutée.
    Ready(ReadyEntry),
    /// Setup abandonné.
    Abandoned(AbandonReason),
    /// Coin en état Idle (aucun setup en cours).
    Idle,
}

/// Tracker de pullback par coin. Instancié une fois, mis à jour chaque tick.
pub struct PullbackTracker {
    states: HashMap<String, Phase>,
}

impl PullbackTracker {
    pub fn new() -> Self {
        Self {
            states: HashMap::new(),
        }
    }

    /// Démarre un nouveau setup de pullback pour un coin.
    ///
    /// Appelé depuis main.rs après que la direction est confirmée (N ticks consécutifs).
    /// Si un setup existant est déjà en cours pour ce coin, il est remplacé.
    ///
    /// `sl_pct` et `tp_pct` : distances relatives calculées par `mfdp::compute_levels()`
    /// extraites de l'Intent (|price - stop_loss| / price).
    pub fn start(
        &mut self,
        coin: &str,
        direction: Direction,
        initial_mid: f64,
        sl_pct: f64,
        tp_pct: f64,
        size: Decimal,
        max_wait_s: u64,
        dir_score: f64,
        now_ms: i64,
        settings: &PullbackSettings,
    ) {
        let expires_at = now_ms + settings.wait_move_ms;
        debug!(
            "[PULLBACK] {} {} setup started: mid={:.4} sl={:.4}% tp={:.4}% move_timeout={}s retrace_timeout={}s",
            coin,
            format!("{:?}", direction),
            initial_mid,
            sl_pct * 100.0,
            tp_pct * 100.0,
            settings.wait_move_ms / 1000,
            settings.wait_retrace_ms / 1000,
        );
        self.states.insert(
            coin.to_string(),
            Phase::WaitingMove {
                direction,
                initial_mid,
                extreme_mid: initial_mid,
                expires_at,
                sl_pct,
                tp_pct,
                size,
                max_wait_s,
                dir_score,
            },
        );
    }

    /// Annule tout setup en cours pour ce coin (appelé quand le bot ouvre une position
    /// via un autre chemin, ou quand le signal change de direction).
    pub fn cancel(&mut self, coin: &str) {
        if self.states.remove(coin).is_some() {
            debug!("[PULLBACK] {} setup cancelled", coin);
        }
    }

    /// Retourne vrai si un setup est en cours pour ce coin.
    pub fn is_pending(&self, coin: &str) -> bool {
        self.states.contains_key(coin)
    }

    /// Met à jour le tracker pour un coin donné à chaque tick.
    ///
    /// `mid` : mid price actuel.
    /// `ofi_10s` : OFI 10s actuel (signé : + = buy pressure, - = sell pressure).
    /// `opposite_signal` : vrai si la stratégie émet un signal opposé ce tick.
    pub fn update(
        &mut self,
        coin: &str,
        mid: f64,
        ofi_10s: f64,
        now_ms: i64,
        opposite_signal: bool,
        settings: &PullbackSettings,
    ) -> UpdateResult {
        let phase = match self.states.get_mut(coin) {
            Some(p) => p,
            None => return UpdateResult::Idle,
        };

        // ── Abandon : signal opposé ──────────────────────────────────────────
        if opposite_signal {
            self.states.remove(coin);
            debug!("[PULLBACK] {} abandoned: opposite signal", coin);
            return UpdateResult::Abandoned(AbandonReason::OppositeSignal);
        }

        match phase.clone() {
            // ────────────────────────────────────────────────────────────────
            Phase::WaitingMove {
                direction,
                initial_mid,
                mut extreme_mid,
                expires_at,
                sl_pct,
                tp_pct,
                size,
                max_wait_s,
                dir_score,
            } => {
                // Abandon : timeout
                if now_ms >= expires_at {
                    self.states.remove(coin);
                    debug!("[PULLBACK] {} timeout in WaitingMove phase", coin);
                    return UpdateResult::Abandoned(AbandonReason::Timeout);
                }

                // Mise à jour de l'extrême
                match direction {
                    Direction::Long => {
                        if mid > extreme_mid {
                            extreme_mid = mid;
                        }
                    }
                    Direction::Short => {
                        if mid < extreme_mid {
                            extreme_mid = mid;
                        }
                    }
                }

                // Vérifier si le micro-move minimum est atteint
                let move_bps = match direction {
                    Direction::Long => (extreme_mid - initial_mid) / initial_mid * 10_000.0,
                    Direction::Short => (initial_mid - extreme_mid) / initial_mid * 10_000.0,
                };

                *phase = Phase::WaitingMove {
                    direction,
                    initial_mid,
                    extreme_mid,
                    expires_at,
                    sl_pct,
                    tp_pct,
                    size,
                    max_wait_s,
                    dir_score,
                };

                if move_bps >= settings.min_move_bps {
                    // Micro-move confirmé → passer en WaitingPullback avec son propre timeout
                    let retrace_expires_at = now_ms + settings.wait_retrace_ms;
                    debug!(
                        "[PULLBACK] {} {} micro-move confirmed: {:.2}bps (min: {:.2}bps) → waiting pullback ({}s)",
                        coin,
                        format!("{:?}", direction),
                        move_bps,
                        settings.min_move_bps,
                        settings.wait_retrace_ms / 1000,
                    );
                    *phase = Phase::WaitingPullback {
                        direction,
                        initial_mid,
                        extreme_mid,
                        expires_at: retrace_expires_at,
                        sl_pct,
                        tp_pct,
                        size,
                        max_wait_s,
                        dir_score,
                    };
                }

                UpdateResult::Waiting
            }

            // ────────────────────────────────────────────────────────────────
            Phase::WaitingPullback {
                direction,
                initial_mid,
                extreme_mid,
                expires_at,
                sl_pct,
                tp_pct,
                size,
                max_wait_s,
                dir_score,
            } => {
                // Abandon : timeout
                if now_ms >= expires_at {
                    self.states.remove(coin);
                    debug!("[PULLBACK] {} timeout in WaitingPullback phase", coin);
                    return UpdateResult::Abandoned(AbandonReason::Timeout);
                }

                let move_size = match direction {
                    Direction::Long => extreme_mid - initial_mid,
                    Direction::Short => initial_mid - extreme_mid,
                };

                // Retrace depuis l'extrême
                let retrace = match direction {
                    Direction::Long => (extreme_mid - mid).max(0.0),
                    Direction::Short => (mid - extreme_mid).max(0.0),
                };

                let retrace_pct = if move_size > 0.0 {
                    retrace / move_size
                } else {
                    0.0
                };

                // Abandon : renversement complet (retrace > 100%)
                if retrace_pct > 1.0 {
                    self.states.remove(coin);
                    debug!(
                        "[PULLBACK] {} abandoned: reversal (retrace={:.0}% > 100%)",
                        coin,
                        retrace_pct * 100.0
                    );
                    return UpdateResult::Abandoned(AbandonReason::Reversal);
                }

                // Vérifier le pullback + confirmation OFI
                let pullback_ok = retrace_pct >= settings.retrace_pct;
                let ofi_ok = match direction {
                    Direction::Long => ofi_10s >= settings.ofi_confirm_threshold,
                    Direction::Short => ofi_10s <= -settings.ofi_confirm_threshold,
                };

                if pullback_ok && ofi_ok {
                    self.states.remove(coin);
                    info!(
                        "[PULLBACK] {} {} READY: extreme={:.4} retrace={:.1}% ofi={:.3} → entry at mid={:.4}",
                        coin,
                        format!("{:?}", direction),
                        extreme_mid,
                        retrace_pct * 100.0,
                        ofi_10s,
                        mid,
                    );
                    return UpdateResult::Ready(ReadyEntry {
                        coin: coin.to_string(),
                        direction,
                        entry_mid: mid,
                        sl_pct,
                        tp_pct,
                        size,
                        max_wait_s,
                        dir_score,
                    });
                }

                // Mise à jour de l'extrême si le prix continue dans la direction
                let new_extreme = match direction {
                    Direction::Long => extreme_mid.max(mid),
                    Direction::Short => extreme_mid.min(mid),
                };
                *phase = Phase::WaitingPullback {
                    direction,
                    initial_mid,
                    extreme_mid: new_extreme,
                    expires_at,
                    sl_pct,
                    tp_pct,
                    size,
                    max_wait_s,
                    dir_score,
                };

                UpdateResult::Waiting
            }
        }
    }
}
