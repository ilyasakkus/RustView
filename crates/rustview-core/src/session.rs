//! Fail-closed session state and input authorization.

use thiserror::Error;

use crate::protocol::{
    InputMessage, PeerMessage, PermissionSet, ProtocolError, SessionGrant, SessionRequest,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionPhase {
    #[default]
    AwaitingRequest,
    AwaitingApproval,
    Active,
    Closed,
}

/// Tracks one peer session and validates every permission-sensitive message.
#[derive(Clone, Debug, Default)]
pub struct SessionState {
    phase: SessionPhase,
    request: Option<SessionRequest>,
    grant: Option<SessionGrant>,
    next_input_sequence: u64,
}

impl SessionState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub const fn phase(&self) -> SessionPhase {
        self.phase
    }

    #[must_use]
    pub fn pending_request(&self) -> Option<&SessionRequest> {
        self.request.as_ref()
    }

    #[must_use]
    pub fn grant(&self) -> Option<&SessionGrant> {
        self.grant.as_ref()
    }

    #[must_use]
    pub fn granted_permissions(&self) -> PermissionSet {
        self.grant
            .as_ref()
            .map_or(PermissionSet::NONE, |grant| grant.granted_permissions)
    }

    #[must_use]
    pub fn can_control(&self) -> bool {
        self.phase == SessionPhase::Active && self.granted_permissions().can_control()
    }

    /// Observes an incoming or outgoing message and applies the state transition only if valid.
    pub fn observe(&mut self, message: &PeerMessage) -> Result<(), SessionError> {
        message.validate()?;
        if self.phase == SessionPhase::Closed {
            return Err(SessionError::Closed);
        }
        match message {
            PeerMessage::SessionRequest(request) => self.register_request(request),
            PeerMessage::SessionGrant(grant) => self.activate(grant),
            PeerMessage::Input(input) => self.authorize_input(input),
            PeerMessage::FrameStart(_) | PeerMessage::FrameChunk(_) => {
                self.require_active_permission(PermissionSet::VIEW_SCREEN)
            }
            PeerMessage::Ping { .. } | PeerMessage::Pong { .. } => Ok(()),
            PeerMessage::Disconnect { .. } => {
                self.close();
                Ok(())
            }
        }
    }

    pub fn register_request(&mut self, request: &SessionRequest) -> Result<(), SessionError> {
        request.validate()?;
        if self.phase != SessionPhase::AwaitingRequest {
            return Err(SessionError::UnexpectedMessage {
                phase: self.phase,
                message: "session request",
            });
        }
        self.request = Some(request.clone());
        self.phase = SessionPhase::AwaitingApproval;
        Ok(())
    }

    pub fn activate(&mut self, grant: &SessionGrant) -> Result<(), SessionError> {
        grant.validate()?;
        if self.phase != SessionPhase::AwaitingApproval {
            return Err(SessionError::UnexpectedMessage {
                phase: self.phase,
                message: "session grant",
            });
        }
        let request = self.request.as_ref().ok_or(SessionError::MissingRequest)?;
        if grant.request_id != request.request_id {
            return Err(SessionError::RequestIdMismatch);
        }
        if !grant
            .granted_permissions
            .is_subset_of(request.requested_permissions)
        {
            return Err(SessionError::PermissionEscalation);
        }
        self.grant = Some(grant.clone());
        self.next_input_sequence = 0;
        self.phase = SessionPhase::Active;
        Ok(())
    }

    pub fn authorize_input(&mut self, input: &InputMessage) -> Result<(), SessionError> {
        input.validate()?;
        let required = input.event.required_permission();
        self.require_active_permission(required)?;
        let grant = self.grant.as_ref().ok_or(SessionError::MissingGrant)?;
        if input.session_id != grant.session_id {
            return Err(SessionError::SessionIdMismatch);
        }
        if input.grant_epoch != grant.grant_epoch {
            return Err(SessionError::GrantEpochMismatch);
        }
        if input.sequence != self.next_input_sequence {
            return Err(SessionError::UnexpectedInputSequence {
                expected: self.next_input_sequence,
                actual: input.sequence,
            });
        }
        self.next_input_sequence = self
            .next_input_sequence
            .checked_add(1)
            .ok_or(SessionError::InputSequenceExhausted)?;
        Ok(())
    }

    pub fn close(&mut self) {
        self.phase = SessionPhase::Closed;
        self.request = None;
        self.grant = None;
        self.next_input_sequence = 0;
    }

    fn require_active_permission(&self, required: PermissionSet) -> Result<(), SessionError> {
        if self.phase != SessionPhase::Active {
            return Err(SessionError::NotActive(self.phase));
        }
        let granted = self.granted_permissions();
        if !granted.contains(required) {
            return Err(SessionError::PermissionDenied { required, granted });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SessionError {
    #[error("invalid protocol value")]
    Protocol(#[from] ProtocolError),
    #[error("session is closed")]
    Closed,
    #[error("unexpected {message} while session is {phase:?}")]
    UnexpectedMessage {
        phase: SessionPhase,
        message: &'static str,
    },
    #[error("session request is missing")]
    MissingRequest,
    #[error("session grant is missing")]
    MissingGrant,
    #[error("grant request ID does not match the pending request")]
    RequestIdMismatch,
    #[error("grant contains permissions that were not requested")]
    PermissionEscalation,
    #[error("session is not active: {0:?}")]
    NotActive(SessionPhase),
    #[error("required permissions {required:?} are not present in {granted:?}")]
    PermissionDenied {
        required: PermissionSet,
        granted: PermissionSet,
    },
    #[error("input session ID does not match the active grant")]
    SessionIdMismatch,
    #[error("input grant epoch does not match the active grant")]
    GrantEpochMismatch,
    #[error("input sequence mismatch: expected {expected}, got {actual}")]
    UnexpectedInputSequence { expected: u64, actual: u64 },
    #[error("input sequence exhausted")]
    InputSequenceExhausted,
}

#[cfg(test)]
mod tests {
    use crate::protocol::{ButtonState, InputEvent};

    use super::*;

    fn request(permissions: PermissionSet) -> SessionRequest {
        SessionRequest {
            request_id: [1; 16],
            viewer_name: "viewer".into(),
            requested_permissions: permissions,
        }
    }

    fn grant(permissions: PermissionSet) -> SessionGrant {
        SessionGrant {
            request_id: [1; 16],
            session_id: [2; 16],
            grant_epoch: 1,
            granted_permissions: permissions,
        }
    }

    fn input(sequence: u64, event: InputEvent) -> InputMessage {
        InputMessage {
            session_id: [2; 16],
            grant_epoch: 1,
            sequence,
            event,
        }
    }

    #[test]
    fn input_is_impossible_before_explicit_grant() {
        let mut state = SessionState::new();
        let event = input(0, InputEvent::MouseMove { x: 10, y: 20 });
        assert!(matches!(
            state.authorize_input(&event),
            Err(SessionError::NotActive(SessionPhase::AwaitingRequest))
        ));

        state
            .register_request(&request(PermissionSet::VIEW_AND_CONTROL))
            .expect("request");
        assert!(matches!(
            state.authorize_input(&event),
            Err(SessionError::NotActive(SessionPhase::AwaitingApproval))
        ));
    }

    #[test]
    fn grant_cannot_escalate_beyond_request() {
        let mut state = SessionState::new();
        state
            .register_request(&request(PermissionSet::VIEW_ONLY))
            .expect("request");
        assert!(matches!(
            state.activate(&grant(PermissionSet::VIEW_AND_CONTROL)),
            Err(SessionError::PermissionEscalation)
        ));
        assert_eq!(state.phase(), SessionPhase::AwaitingApproval);
    }

    #[test]
    fn pointer_grant_does_not_authorize_keyboard() {
        let permissions = PermissionSet::VIEW_SCREEN | PermissionSet::CONTROL_POINTER;
        let mut state = SessionState::new();
        state
            .register_request(&request(permissions))
            .expect("request");
        state.activate(&grant(permissions)).expect("grant");
        state
            .authorize_input(&input(0, InputEvent::MouseMove { x: 1, y: 2 }))
            .expect("pointer authorized");

        let key = input(
            1,
            InputEvent::Key {
                usage: 4,
                state: ButtonState::Pressed,
            },
        );
        assert!(matches!(
            state.authorize_input(&key),
            Err(SessionError::PermissionDenied { .. })
        ));
    }

    #[test]
    fn replay_stale_epoch_and_wrong_session_are_rejected() {
        let mut state = SessionState::new();
        state
            .register_request(&request(PermissionSet::VIEW_AND_CONTROL))
            .expect("request");
        state
            .activate(&grant(PermissionSet::VIEW_AND_CONTROL))
            .expect("grant");
        let first = input(0, InputEvent::MouseMove { x: 1, y: 2 });
        state.authorize_input(&first).expect("first input");
        assert!(matches!(
            state.authorize_input(&first),
            Err(SessionError::UnexpectedInputSequence { .. })
        ));

        let mut stale = input(1, InputEvent::MouseMove { x: 1, y: 2 });
        stale.grant_epoch = 2;
        assert!(matches!(
            state.authorize_input(&stale),
            Err(SessionError::GrantEpochMismatch)
        ));
        stale.grant_epoch = 1;
        stale.session_id = [3; 16];
        assert!(matches!(
            state.authorize_input(&stale),
            Err(SessionError::SessionIdMismatch)
        ));
    }

    #[test]
    fn disconnect_revokes_authorization_immediately() {
        let mut state = SessionState::new();
        state
            .register_request(&request(PermissionSet::VIEW_AND_CONTROL))
            .expect("request");
        state
            .activate(&grant(PermissionSet::VIEW_AND_CONTROL))
            .expect("grant");
        state.close();
        assert!(!state.can_control());
        assert!(matches!(
            state.authorize_input(&input(0, InputEvent::MouseMove { x: 1, y: 2 })),
            Err(SessionError::NotActive(SessionPhase::Closed))
        ));
    }
}
