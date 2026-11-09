//! Consumer-local user lookup and application annotations. No Host mutation,
//! physical device identity, network request, callback or persistence is implied.
use crate::{SourceId, UserId, UserView};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, io::Write};

pub const TOOLKIT_VERSION: u32 = 1;
pub const MAX_TOOLKIT_REQUEST_BYTES: usize = 8192;
pub const MAX_USER_DATA_BYTES: usize = 4096;
pub const MAX_TOOLKIT_DATA_BYTES: usize = 32768;
pub const MAX_PALETTE_COLORS: usize = 32;
const DEFAULT_PALETTE: [[u8; 4]; 8] = [
    [29, 122, 140, 255],
    [173, 72, 109, 255],
    [109, 90, 179, 255],
    [140, 102, 32, 255],
    [44, 133, 83, 255],
    [62, 104, 175, 255],
    [180, 79, 44, 255],
    [117, 112, 123, 255],
];

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolkitRequest {
    pub version: u32,
    pub operation: ToolkitOperation,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolkitOperation {
    List,
    User { user: UserId },
    Source { source: SourceId },
    SetData { user: UserId, data: Value },
    ClearData { user: UserId },
    Palette,
    SetPalette { colors: Vec<[u8; 4]> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserContext {
    pub user: UserView,
    pub app_data: Option<Value>,
    /// An application badge color, separate from the observed Host cursor color.
    pub badge_rgba: [u8; 4],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolkitResponse {
    pub version: u32,
    pub result: ToolkitResult,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolkitResult {
    Users { users: Vec<UserContext> },
    User { user: Option<Box<UserContext>> },
    DataUpdated { user: UserId },
    Palette { colors: Vec<[u8; 4]> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolkitError {
    Invalid,
    NotLive,
    MissingUser,
    Limit,
}
impl std::fmt::Display for ToolkitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Invalid => "Invalid local toolkit request",
            Self::NotLive => "Local toolkit requires a current Live authorized snapshot",
            Self::MissingUser => "User is not in the current authorized snapshot",
            Self::Limit => "Local toolkit limit exceeded",
        })
    }
}
impl std::error::Error for ToolkitError {}

#[derive(Debug, Clone, PartialEq)]
struct Annotation {
    source: SourceId,
    data: Value,
    bytes: usize,
}
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct UserToolkit {
    epoch: Option<u64>,
    data: BTreeMap<UserId, Annotation>,
    palette: Vec<[u8; 4]>,
}
impl Default for UserToolkit {
    fn default() -> Self {
        Self {
            epoch: None,
            data: BTreeMap::new(),
            palette: DEFAULT_PALETTE.to_vec(),
        }
    }
}
impl UserToolkit {
    pub(crate) fn reconcile(&mut self, epoch: u64, users: &BTreeMap<UserId, UserView>) {
        if self.epoch != Some(epoch) {
            *self = Self::default();
            self.epoch = Some(epoch);
        }
        self.data.retain(|id, annotation| {
            users
                .get(id)
                .is_some_and(|user| user.source == annotation.source)
        });
    }
    pub(crate) fn remove(&mut self, user: UserId) {
        self.data.remove(&user);
    }
    fn context(&self, user: &UserView) -> UserContext {
        // Fixed-width integer bytes keep results identical in native Rust and WASM.
        // Palette slots may repeat; the palette is a bounded set of badge colors.
        let mut hash = 0xcbf29ce484222325_u64;
        for byte in user
            .id
            .epoch
            .to_le_bytes()
            .into_iter()
            .chain(user.id.local.to_le_bytes())
        {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
        }
        UserContext {
            user: user.clone(),
            app_data: self.data.get(&user.id).map(|entry| entry.data.clone()),
            badge_rgba: self.palette[(hash % self.palette.len() as u64) as usize],
        }
    }
    pub(crate) fn execute(
        &mut self,
        users: &BTreeMap<UserId, UserView>,
        operation: ToolkitOperation,
    ) -> Result<ToolkitResponse, ToolkitError> {
        let result = match operation {
            ToolkitOperation::List => ToolkitResult::Users {
                users: users.values().map(|user| self.context(user)).collect(),
            },
            ToolkitOperation::User { user } => ToolkitResult::User {
                user: users.get(&user).map(|user| Box::new(self.context(user))),
            },
            ToolkitOperation::Source { source } => ToolkitResult::User {
                user: users
                    .values()
                    .find(|user| user.source == source)
                    .map(|user| Box::new(self.context(user))),
            },
            ToolkitOperation::SetData { user, data } => {
                let view = users.get(&user).ok_or(ToolkitError::MissingUser)?;
                let bytes = data_size(&data)?;
                let previous = self.data.get(&user).map_or(0, |entry| entry.bytes);
                let total: usize = self.data.values().map(|entry| entry.bytes).sum();
                if total - previous + bytes > MAX_TOOLKIT_DATA_BYTES
                    || (!self.data.contains_key(&user) && self.data.len() >= crate::MAX_USERS)
                {
                    return Err(ToolkitError::Limit);
                }
                self.data.insert(
                    user,
                    Annotation {
                        source: view.source,
                        data,
                        bytes,
                    },
                );
                ToolkitResult::DataUpdated { user }
            }
            ToolkitOperation::ClearData { user } => {
                if !users.contains_key(&user) {
                    return Err(ToolkitError::MissingUser);
                }
                self.data.remove(&user);
                ToolkitResult::DataUpdated { user }
            }
            ToolkitOperation::Palette => ToolkitResult::Palette {
                colors: self.palette.clone(),
            },
            ToolkitOperation::SetPalette { colors } => {
                if colors.is_empty()
                    || colors.len() > MAX_PALETTE_COLORS
                    || colors.iter().any(|color| color[3] == 0)
                {
                    return Err(ToolkitError::Invalid);
                }
                self.palette = colors;
                ToolkitResult::Palette {
                    colors: self.palette.clone(),
                }
            }
        };
        Ok(ToolkitResponse {
            version: TOOLKIT_VERSION,
            result,
        })
    }
}

fn data_size(value: &Value) -> Result<usize, ToolkitError> {
    // Bound nesting and node count even for programmatically constructed Values.
    let mut pending = vec![(value, 0)];
    let mut nodes = 0;
    while let Some((value, depth)) = pending.pop() {
        nodes += 1;
        if depth > 32 || nodes > MAX_USER_DATA_BYTES {
            return Err(ToolkitError::Limit);
        }
        match value {
            Value::String(text) if text.len() > MAX_USER_DATA_BYTES => {
                return Err(ToolkitError::Limit);
            }
            Value::Array(values) => {
                if values.len() + pending.len() > MAX_USER_DATA_BYTES {
                    return Err(ToolkitError::Limit);
                }
                pending.extend(values.iter().map(|value| (value, depth + 1)));
            }
            Value::Object(values) => {
                if values.len() + pending.len() > MAX_USER_DATA_BYTES
                    || values.keys().any(|key| key.len() > MAX_USER_DATA_BYTES)
                {
                    return Err(ToolkitError::Limit);
                }
                pending.extend(values.values().map(|value| (value, depth + 1)));
            }
            _ => {}
        }
    }
    struct Budget(usize);
    impl Write for Budget {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAX_USER_DATA_BYTES - self.0 {
                return Err(std::io::Error::other("local data limit"));
            }
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut budget = Budget(0);
    serde_json::to_writer(&mut budget, value).map_err(|_| ToolkitError::Limit)?;
    Ok(budget.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ApplicationMode, ClientState, Envelope, Event, Poll, ScreenState, Snapshot};
    use mux_core::desktop::Bounds;
    use serde_json::json;

    const EPOCH: u64 = 9_007_199_254_740_993;
    fn snapshot(epoch: u64, count: u64) -> Snapshot {
        let bounds = Bounds {
            left: -100,
            top: 0,
            right: 100,
            bottom: 100,
        };
        Snapshot {
            version: 1,
            epoch,
            through_sequence: 0,
            hosted: None,
            users: (1..=count)
                .map(|local| UserView {
                    id: UserId { epoch, local },
                    owned_by_recipient: false,
                    name: format!("User {local}"),
                    source: SourceId(EPOCH + local),
                    pointer: None,
                    cursor: None,
                    held_keys: Some(Vec::new()),
                })
                .collect(),
            screens: ScreenState {
                revision: 1,
                desktop: bounds,
                screens: vec![bounds],
            },
            windows: Vec::new(),
            capabilities: Vec::new(),
            mode: ApplicationMode::Cooperative,
        }
    }
    fn live(count: u64) -> ClientState {
        let mut model = ClientState::default();
        model
            .install_snapshot(snapshot(EPOCH, count), EPOCH)
            .unwrap();
        model
    }
    fn run(
        model: &mut ClientState,
        operation: ToolkitOperation,
    ) -> Result<ToolkitResult, ToolkitError> {
        model
            .local_toolkit(ToolkitRequest {
                version: 1,
                operation,
            })
            .map(|reply| reply.result)
    }
    fn context(model: &mut ClientState, user: UserId) -> Option<Box<UserContext>> {
        let ToolkitResult::User { user } = run(model, ToolkitOperation::User { user }).unwrap()
        else {
            panic!("user result");
        };
        user
    }
    fn set(
        model: &mut ClientState,
        local: u64,
        data: Value,
    ) -> Result<ToolkitResult, ToolkitError> {
        run(
            model,
            ToolkitOperation::SetData {
                user: UserId {
                    epoch: EPOCH,
                    local,
                },
                data,
            },
        )
    }

    #[test]
    fn lookup_uses_exact_authorized_u64_identities_and_requires_live_state() {
        let mut model = live(2);
        let request = format!(
            r#"{{"version":1,"operation":{{"kind":"source","source":{}}}}}"#,
            EPOCH + 1
        );
        let response: ToolkitResponse =
            serde_json::from_str(&model.local_toolkit_json(&request).unwrap()).unwrap();
        let ToolkitResult::User { user: Some(user) } = response.result else {
            panic!("source lookup")
        };
        assert_eq!(
            user.user.id,
            UserId {
                epoch: EPOCH,
                local: 1
            }
        );
        assert_eq!(
            run(
                &mut model,
                ToolkitOperation::Source {
                    source: SourceId(0)
                }
            )
            .unwrap(),
            ToolkitResult::User { user: None }
        );
        assert!(
            context(
                &mut model,
                UserId {
                    epoch: EPOCH - 1,
                    local: 1
                }
            )
            .is_none()
        );
        assert_eq!(
            set(&mut model, 99, json!(true)),
            Err(ToolkitError::MissingUser)
        );
        assert_eq!(
            model.local_toolkit_json(r#"{"version":2,"operation":{"kind":"list"}}"#),
            Err(ToolkitError::Invalid)
        );
        model.disconnect();
        assert_eq!(
            run(&mut model, ToolkitOperation::List),
            Err(ToolkitError::NotLive)
        );
    }

    #[test]
    fn data_is_local_bounded_and_rejected_writes_leave_existing_values_intact() {
        let mut first = live(12);
        let mut second = live(12);
        let id = UserId {
            epoch: EPOCH,
            local: 1,
        };
        set(&mut first, 1, json!({"client": "native"})).unwrap();
        assert!(context(&mut second, id).unwrap().app_data.is_none());
        assert_eq!(
            set(&mut first, 1, json!("x".repeat(MAX_USER_DATA_BYTES))),
            Err(ToolkitError::Limit)
        );
        assert_eq!(
            context(&mut first, id).unwrap().app_data,
            Some(json!({"client": "native"}))
        );
        for local in 1..=8 {
            set(&mut first, local, json!("x".repeat(4000))).unwrap();
        }
        assert_eq!(
            set(&mut first, 9, json!("x".repeat(4000))),
            Err(ToolkitError::Limit)
        );
        assert!(
            context(
                &mut first,
                UserId {
                    epoch: EPOCH,
                    local: 9
                }
            )
            .unwrap()
            .app_data
            .is_none()
        );
        run(&mut first, ToolkitOperation::ClearData { user: id }).unwrap();
        set(&mut first, 9, json!("x".repeat(4000))).unwrap();
        assert_eq!(
            first.local_toolkit_json(&" ".repeat(MAX_TOOLKIT_REQUEST_BYTES + 1)),
            Err(ToolkitError::Limit)
        );
        let mut nested = Value::Null;
        for _ in 0..34 {
            nested = json!([nested]);
        }
        assert_eq!(set(&mut second, 1, nested), Err(ToolkitError::Limit));
    }

    #[test]
    fn refresh_prunes_removed_or_rebound_users_but_preserves_live_rename_data() {
        let mut model = live(2);
        let id = UserId {
            epoch: EPOCH,
            local: 1,
        };
        set(&mut model, 1, json!("application state")).unwrap();
        let mut fresh = snapshot(EPOCH, 2);
        fresh.users[0].name = "Renamed".into();
        model.install_snapshot(fresh.clone(), EPOCH).unwrap();
        assert_eq!(
            context(&mut model, id).unwrap().app_data,
            Some(json!("application state"))
        );
        fresh.users[0].source = SourceId(123);
        model.install_snapshot(fresh, EPOCH).unwrap();
        assert!(context(&mut model, id).unwrap().app_data.is_none());
        set(&mut model, 1, json!("new binding")).unwrap();
        model.install_snapshot(snapshot(EPOCH, 0), EPOCH).unwrap();
        model.install_snapshot(snapshot(EPOCH, 2), EPOCH).unwrap();
        assert!(context(&mut model, id).unwrap().app_data.is_none());
    }

    #[test]
    fn disposal_gap_revocation_and_epoch_changes_erase_application_state() {
        let mut model = live(1);
        let id = UserId {
            epoch: EPOCH,
            local: 1,
        };
        set(&mut model, 1, json!(123)).unwrap();
        let user = model.users()[&id].clone();
        model
            .fold_poll(Poll::Events {
                events: vec![
                    Envelope {
                        version: 1,
                        epoch: EPOCH,
                        sequence: 1,
                        event: Event::UserDisposed { user: id },
                    },
                    Envelope {
                        version: 1,
                        epoch: EPOCH,
                        sequence: 2,
                        event: Event::UserCreated { user },
                    },
                ],
            })
            .unwrap();
        assert!(context(&mut model, id).unwrap().app_data.is_none());
        set(&mut model, 1, json!(456)).unwrap();
        model
            .fold_poll(Poll::Gap {
                after_sequence: 2,
                lost_through: 3,
            })
            .unwrap();
        assert_eq!(
            run(&mut model, ToolkitOperation::List),
            Err(ToolkitError::NotLive)
        );
        let mut fresh = snapshot(EPOCH, 1);
        fresh.through_sequence = 3;
        model.install_snapshot(fresh, EPOCH).unwrap();
        assert!(context(&mut model, id).unwrap().app_data.is_none());
        set(&mut model, 1, json!(789)).unwrap();
        model
            .fold_poll(Poll::Ended {
                reason: crate::EndReason::Revoked,
            })
            .unwrap();
        model
            .install_snapshot(snapshot(EPOCH + 1, 1), EPOCH + 1)
            .unwrap();
        assert!(
            context(
                &mut model,
                UserId {
                    epoch: EPOCH + 1,
                    local: 1
                }
            )
            .unwrap()
            .app_data
            .is_none()
        );
    }

    #[test]
    fn badge_palette_is_deterministic_bounded_and_never_changes_observed_cursors() {
        let mut first = live(2);
        let mut second = live(2);
        let id = UserId {
            epoch: EPOCH,
            local: 1,
        };
        let observed = first.users().clone();
        assert_eq!(
            context(&mut first, id).unwrap().badge_rgba,
            context(&mut second, id).unwrap().badge_rgba
        );
        run(
            &mut first,
            ToolkitOperation::SetPalette {
                colors: vec![[1, 2, 3, 255]],
            },
        )
        .unwrap();
        assert_eq!(context(&mut first, id).unwrap().badge_rgba, [1, 2, 3, 255]);
        assert_eq!(first.users(), &observed);
        for colors in [
            vec![],
            vec![[1, 2, 3, 0]],
            vec![[0, 0, 0, 255]; MAX_PALETTE_COLORS + 1],
        ] {
            assert_eq!(
                run(&mut first, ToolkitOperation::SetPalette { colors }),
                Err(ToolkitError::Invalid)
            );
        }
        assert_eq!(context(&mut first, id).unwrap().badge_rgba, [1, 2, 3, 255]);
        first.disconnect();
        first.install_snapshot(snapshot(EPOCH, 2), EPOCH).unwrap();
        assert_eq!(
            context(&mut first, id).unwrap().badge_rgba,
            context(&mut second, id).unwrap().badge_rgba
        );
    }
}
