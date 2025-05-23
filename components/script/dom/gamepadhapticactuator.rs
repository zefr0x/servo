/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::cell::Cell;
use std::rc::Rc;

use dom_struct::dom_struct;
use embedder_traits::{DualRumbleEffectParams, EmbedderMsg, GamepadSupportedHapticEffects};
use ipc_channel::ipc;
use ipc_channel::router::ROUTER;
use js::rust::MutableHandleValue;

use crate::dom::bindings::cell::DomRefCell;
use crate::dom::bindings::codegen::Bindings::GamepadHapticActuatorBinding::{
    GamepadEffectParameters, GamepadHapticActuatorMethods, GamepadHapticEffectType,
};
use crate::dom::bindings::codegen::Bindings::WindowBinding::Window_Binding::WindowMethods;
use crate::dom::bindings::error::Error;
use crate::dom::bindings::refcounted::{Trusted, TrustedPromise};
use crate::dom::bindings::reflector::{DomGlobal, Reflector, reflect_dom_object};
use crate::dom::bindings::root::DomRoot;
use crate::dom::bindings::str::DOMString;
use crate::dom::bindings::utils::to_frozen_array;
use crate::dom::promise::Promise;
use crate::dom::window::Window;
use crate::realms::InRealm;
use crate::script_runtime::{CanGc, JSContext};
use crate::task_source::SendableTaskSource;

struct HapticEffectListener {
    task_source: SendableTaskSource,
    context: Trusted<GamepadHapticActuator>,
}

impl HapticEffectListener {
    fn handle_stopped(&self, stopped_successfully: bool) {
        let context = self.context.clone();
        self.task_source
            .queue(task!(handle_haptic_effect_stopped: move || {
                let actuator = context.root();
                actuator.handle_haptic_effect_stopped(stopped_successfully);
            }));
    }

    fn handle_completed(&self, completed_successfully: bool) {
        let context = self.context.clone();
        self.task_source
            .queue(task!(handle_haptic_effect_completed: move || {
                let actuator = context.root();
                actuator.handle_haptic_effect_completed(completed_successfully, CanGc::note());
            }));
    }
}

/// <https://www.w3.org/TR/gamepad/#gamepadhapticactuator-interface>
#[dom_struct]
pub(crate) struct GamepadHapticActuator {
    reflector_: Reflector,
    gamepad_index: u32,
    /// <https://www.w3.org/TR/gamepad/#dfn-effects>
    effects: Vec<GamepadHapticEffectType>,
    /// <https://www.w3.org/TR/gamepad/#dfn-playingeffectpromise>
    #[ignore_malloc_size_of = "Rc is hard"]
    playing_effect_promise: DomRefCell<Option<Rc<Promise>>>,
    /// The current sequence ID for playing effects,
    /// incremented on every call to playEffect() or reset().
    /// Used to ensure that promises are resolved correctly.
    /// Based on this pending PR <https://github.com/w3c/gamepad/pull/201>
    sequence_id: Cell<u32>,
    /// The sequence ID during the last playEffect() call
    effect_sequence_id: Cell<u32>,
    /// The sequence ID during the last reset() call
    reset_sequence_id: Cell<u32>,
}

impl GamepadHapticActuator {
    fn new_inherited(
        gamepad_index: u32,
        supported_haptic_effects: GamepadSupportedHapticEffects,
    ) -> GamepadHapticActuator {
        let mut effects = vec![];
        if supported_haptic_effects.supports_dual_rumble {
            effects.push(GamepadHapticEffectType::Dual_rumble);
        }
        if supported_haptic_effects.supports_trigger_rumble {
            effects.push(GamepadHapticEffectType::Trigger_rumble);
        }
        Self {
            reflector_: Reflector::new(),
            gamepad_index,
            effects,
            playing_effect_promise: DomRefCell::new(None),
            sequence_id: Cell::new(0),
            effect_sequence_id: Cell::new(0),
            reset_sequence_id: Cell::new(0),
        }
    }

    pub(crate) fn new(
        window: &Window,
        gamepad_index: u32,
        supported_haptic_effects: GamepadSupportedHapticEffects,
        can_gc: CanGc,
    ) -> DomRoot<GamepadHapticActuator> {
        reflect_dom_object(
            Box::new(GamepadHapticActuator::new_inherited(
                gamepad_index,
                supported_haptic_effects,
            )),
            window,
            can_gc,
        )
    }
}

impl GamepadHapticActuatorMethods<crate::DomTypeHolder> for GamepadHapticActuator {
    /// <https://www.w3.org/TR/gamepad/#dom-gamepadhapticactuator-effects>
    fn Effects(&self, cx: JSContext, can_gc: CanGc, retval: MutableHandleValue) {
        to_frozen_array(self.effects.as_slice(), cx, retval, can_gc)
    }

    /// <https://www.w3.org/TR/gamepad/#dom-gamepadhapticactuator-playeffect>
    fn PlayEffect(
        &self,
        type_: GamepadHapticEffectType,
        params: &GamepadEffectParameters,
        comp: InRealm,
        can_gc: CanGc,
    ) -> Rc<Promise> {
        let playing_effect_promise = Promise::new_in_current_realm(comp, can_gc);

        // <https://www.w3.org/TR/gamepad/#dfn-valid-effect>
        match type_ {
            // <https://www.w3.org/TR/gamepad/#dfn-valid-dual-rumble-effect>
            GamepadHapticEffectType::Dual_rumble => {
                if *params.strongMagnitude < 0.0 || *params.strongMagnitude > 1.0 {
                    playing_effect_promise.reject_error(
                        Error::Type(
                            "Strong magnitude value is not within range of 0.0 to 1.0.".to_string(),
                        ),
                        can_gc,
                    );
                    return playing_effect_promise;
                } else if *params.weakMagnitude < 0.0 || *params.weakMagnitude > 1.0 {
                    playing_effect_promise.reject_error(
                        Error::Type(
                            "Weak magnitude value is not within range of 0.0 to 1.0.".to_string(),
                        ),
                        can_gc,
                    );
                    return playing_effect_promise;
                }
            },
            // <https://www.w3.org/TR/gamepad/#dfn-valid-trigger-rumble-effect>
            GamepadHapticEffectType::Trigger_rumble => {
                if *params.strongMagnitude < 0.0 || *params.strongMagnitude > 1.0 {
                    playing_effect_promise.reject_error(
                        Error::Type(
                            "Strong magnitude value is not within range of 0.0 to 1.0.".to_string(),
                        ),
                        can_gc,
                    );
                    return playing_effect_promise;
                } else if *params.weakMagnitude < 0.0 || *params.weakMagnitude > 1.0 {
                    playing_effect_promise.reject_error(
                        Error::Type(
                            "Weak magnitude value is not within range of 0.0 to 1.0.".to_string(),
                        ),
                        can_gc,
                    );
                    return playing_effect_promise;
                } else if *params.leftTrigger < 0.0 || *params.leftTrigger > 1.0 {
                    playing_effect_promise.reject_error(
                        Error::Type(
                            "Left trigger value is not within range of 0.0 to 1.0.".to_string(),
                        ),
                        can_gc,
                    );
                    return playing_effect_promise;
                } else if *params.rightTrigger < 0.0 || *params.rightTrigger > 1.0 {
                    playing_effect_promise.reject_error(
                        Error::Type(
                            "Right trigger value is not within range of 0.0 to 1.0.".to_string(),
                        ),
                        can_gc,
                    );
                    return playing_effect_promise;
                }
            },
        }

        let document = self.global().as_window().Document();
        if !document.is_fully_active() {
            playing_effect_promise.reject_error(Error::InvalidState, can_gc);
        }

        self.sequence_id.set(self.sequence_id.get().wrapping_add(1));

        if let Some(promise) = self.playing_effect_promise.borrow_mut().take() {
            let trusted_promise = TrustedPromise::new(promise);
            self.global().task_manager().gamepad_task_source().queue(
                task!(preempt_promise: move || {
                    let promise = trusted_promise.root();
                    let message = DOMString::from("preempted");
                    promise.resolve_native(&message, CanGc::note());
                }),
            );
        }

        if !self.effects.contains(&type_) {
            playing_effect_promise.reject_error(Error::NotSupported, can_gc);
            return playing_effect_promise;
        }

        *self.playing_effect_promise.borrow_mut() = Some(playing_effect_promise.clone());
        self.effect_sequence_id.set(self.sequence_id.get());

        let context = Trusted::new(self);
        let (effect_complete_sender, effect_complete_receiver) =
            ipc::channel().expect("ipc channel failure");
        let listener = HapticEffectListener {
            task_source: self.global().task_manager().gamepad_task_source().into(),
            context,
        };

        ROUTER.add_typed_route(
            effect_complete_receiver,
            Box::new(move |message| match message {
                Ok(msg) => listener.handle_completed(msg),
                Err(err) => warn!("Error receiving a GamepadMsg: {:?}", err),
            }),
        );

        // Note: The spec says we SHOULD also pass a playEffectTimestamp for more precise playback timing
        // when start_delay is non-zero, but this is left more as a footnote without much elaboration.
        // <https://www.w3.org/TR/gamepad/#dfn-issue-a-haptic-effect>

        let params = DualRumbleEffectParams {
            duration: params.duration as f64,
            start_delay: params.startDelay as f64,
            strong_magnitude: *params.strongMagnitude,
            weak_magnitude: *params.weakMagnitude,
        };
        let event = EmbedderMsg::PlayGamepadHapticEffect(
            document.webview_id(),
            self.gamepad_index as usize,
            embedder_traits::GamepadHapticEffectType::DualRumble(params),
            effect_complete_sender,
        );
        self.global().as_window().send_to_embedder(event);

        playing_effect_promise
    }

    /// <https://www.w3.org/TR/gamepad/#dom-gamepadhapticactuator-reset>
    fn Reset(&self, comp: InRealm, can_gc: CanGc) -> Rc<Promise> {
        let promise = Promise::new_in_current_realm(comp, can_gc);

        let document = self.global().as_window().Document();
        if !document.is_fully_active() {
            promise.reject_error(Error::InvalidState, can_gc);
            return promise;
        }

        self.sequence_id.set(self.sequence_id.get().wrapping_add(1));

        if let Some(promise) = self.playing_effect_promise.borrow_mut().take() {
            let trusted_promise = TrustedPromise::new(promise);
            self.global().task_manager().gamepad_task_source().queue(
                task!(preempt_promise: move || {
                    let promise = trusted_promise.root();
                    let message = DOMString::from("preempted");
                    promise.resolve_native(&message, CanGc::note());
                }),
            );
        }

        *self.playing_effect_promise.borrow_mut() = Some(promise.clone());

        self.reset_sequence_id.set(self.sequence_id.get());

        let context = Trusted::new(self);
        let (effect_stop_sender, effect_stop_receiver) =
            ipc::channel().expect("ipc channel failure");
        let listener = HapticEffectListener {
            task_source: self.global().task_manager().gamepad_task_source().into(),
            context,
        };

        ROUTER.add_typed_route(
            effect_stop_receiver,
            Box::new(move |message| match message {
                Ok(msg) => listener.handle_stopped(msg),
                Err(err) => warn!("Error receiving a GamepadMsg: {:?}", err),
            }),
        );

        let event = EmbedderMsg::StopGamepadHapticEffect(
            document.webview_id(),
            self.gamepad_index as usize,
            effect_stop_sender,
        );
        self.global().as_window().send_to_embedder(event);

        self.playing_effect_promise.borrow().clone().unwrap()
    }
}

impl GamepadHapticActuator {
    /// <https://www.w3.org/TR/gamepad/#dom-gamepadhapticactuator-playeffect>
    /// We are in the task queued by the "in-parallel" steps.
    pub(crate) fn handle_haptic_effect_completed(
        &self,
        completed_successfully: bool,
        can_gc: CanGc,
    ) {
        if self.effect_sequence_id.get() != self.sequence_id.get() || !completed_successfully {
            return;
        }
        let playing_effect_promise = self.playing_effect_promise.borrow_mut().take();
        if let Some(promise) = playing_effect_promise {
            let message = DOMString::from("complete");
            promise.resolve_native(&message, can_gc);
        }
    }

    /// <https://www.w3.org/TR/gamepad/#dom-gamepadhapticactuator-reset>
    /// We are in the task queued by the "in-parallel" steps.
    pub(crate) fn handle_haptic_effect_stopped(&self, stopped_successfully: bool) {
        if !stopped_successfully {
            return;
        }

        let playing_effect_promise = self.playing_effect_promise.borrow_mut().take();

        if let Some(promise) = playing_effect_promise {
            let trusted_promise = TrustedPromise::new(promise);
            let sequence_id = self.sequence_id.get();
            let reset_sequence_id = self.reset_sequence_id.get();
            self.global().task_manager().gamepad_task_source().queue(
                task!(complete_promise: move || {
                    if sequence_id != reset_sequence_id {
                        warn!("Mismatched sequence/reset sequence ids: {} != {}", sequence_id, reset_sequence_id);
                        return;
                    }
                    let promise = trusted_promise.root();
                    let message = DOMString::from("complete");
                    promise.resolve_native(&message, CanGc::note());
                })
            );
        }
    }

    /// <https://www.w3.org/TR/gamepad/#handling-visibility-change>
    pub(crate) fn handle_visibility_change(&self) {
        if self.playing_effect_promise.borrow().is_none() {
            return;
        }

        let this = Trusted::new(self);
        self.global().task_manager().gamepad_task_source().queue(
            task!(stop_playing_effect: move || {
                let actuator = this.root();
                let Some(promise) = actuator.playing_effect_promise.borrow_mut().take() else {
                    return;
                };
                let message = DOMString::from("preempted");
                promise.resolve_native(&message, CanGc::note());
            }),
        );

        let (send, _rcv) = ipc::channel().expect("ipc channel failure");

        let document = self.global().as_window().Document();
        let event = EmbedderMsg::StopGamepadHapticEffect(
            document.webview_id(),
            self.gamepad_index as usize,
            send,
        );
        self.global().as_window().send_to_embedder(event);
    }
}
