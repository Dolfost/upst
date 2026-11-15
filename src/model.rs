//! What upst knows about one UPS: the state `upst_core` keeps (variables,
//! status, history, events), plus what only the dashboard needs.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::{Deref, DerefMut};
use std::time::SystemTime;

use crate::history::History;

pub use upst_core::nut::{CmdInfo, VarInfo, dangerous};
pub use upst_core::{Link, from_unix, unix};

/// One `LIST VAR` answer and when it came.
pub type Poll = (Vec<(String, String)>, SystemTime);

/// A UPS on screen. Derefs to the core state.
pub struct Ups {
    pub core: upst_core::Ups,
    /// Index of its server in `App::servers`.
    pub server: usize,
    pub desc: String,
    pub cmds: Vec<CmdInfo>,
    pub rw: BTreeSet<String>,
    pub info: HashMap<String, VarInfo>,
    pub info_pending: HashSet<String>,
    /// Polls that arrived while recorded history was on its way; they are
    /// applied after it.
    pub held: Option<Vec<Poll>>,
    /// Where recorded history came from, and how far back it goes.
    pub recorded: Option<(String, SystemTime)>,
}

impl Ups {
    pub fn new(
        server: usize,
        server_label: &str,
        name: &str,
        desc: &str,
        history: History,
        capacity: Option<f64>,
    ) -> Self {
        let core = upst_core::Ups::builder(name, server_label).capacity_wh(capacity).history(Some(history)).build();
        Ups {
            core,
            server,
            desc: desc.to_string(),
            cmds: Vec::new(),
            rw: BTreeSet::new(),
            info: HashMap::new(),
            info_pending: HashSet::new(),
            held: None,
            recorded: None,
        }
    }
}

impl Deref for Ups {
    type Target = upst_core::Ups;

    fn deref(&self) -> &upst_core::Ups {
        &self.core
    }
}

impl DerefMut for Ups {
    fn deref_mut(&mut self) -> &mut upst_core::Ups {
        &mut self.core
    }
}
