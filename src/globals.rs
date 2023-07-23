use crate::comms::{RelayJob, ToMinionMessage, ToOverlordMessage};
use crate::db::DbRelay;
use crate::delegation::Delegation;
use crate::events::Events;
use crate::feed::Feed;
use crate::fetcher::Fetcher;
use crate::media::Media;
use crate::people::{DbPerson, People};
use crate::relationship::Relationship;
use crate::relay_picker_hooks::Hooks;
use crate::settings::Settings;
use crate::signer::Signer;
use crate::status::StatusQueue;
use crate::storage::Storage;
use dashmap::DashMap;
use gossip_relay_picker::RelayPicker;
use nostr_types::{
    Event, Id, MilliSatoshi, PayRequestData, Profile, PublicKey, PublicKeyHex, RelayUrl,
    UncheckedUrl,
};
use parking_lot::RwLock as PRwLock;
use regex::Regex;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize};
use tokio::sync::{broadcast, mpsc, Mutex, RwLock};

#[derive(Debug, Clone)]
pub enum ZapState {
    None,
    CheckingLnurl(Id, PublicKey, UncheckedUrl),
    SeekingAmount(Id, PublicKey, PayRequestData, UncheckedUrl),
    LoadingInvoice(Id, PublicKey),
    ReadyToPay(Id, String), // String is the Zap Invoice as a string, to be shown as a QR code
}

/// Only one of these is ever created, via lazy_static!, and represents
/// global state for the rust application
pub struct Globals {
    /// Is this the first run?
    pub first_run: AtomicBool,

    /// This is our connection to SQLite. Only one thread at a time.
    pub db: Mutex<Connection>,

    /// This is a broadcast channel. All Minions should listen on it.
    /// To create a receiver, just run .subscribe() on it.
    pub to_minions: broadcast::Sender<ToMinionMessage>,

    /// This is a mpsc channel. The Overlord listens on it.
    /// To create a sender, just clone() it.
    pub to_overlord: mpsc::UnboundedSender<ToOverlordMessage>,

    /// This is ephemeral. It is filled during lazy_static initialization,
    /// and stolen away when the Overlord is created.
    pub tmp_overlord_receiver: Mutex<Option<mpsc::UnboundedReceiver<ToOverlordMessage>>>,

    /// All nostr events currently in memory, keyed by the event Id, as well as
    /// information about if they are new or not, and functions
    pub events: Events,

    /// Events coming in from relays that are not processed yet
    /// stored with Url they came from and Subscription they came in on
    pub incoming_events: RwLock<Vec<(Event, RelayUrl, Option<String>)>>,

    /// All relationships between events
    pub relationships: RwLock<HashMap<Id, Vec<(Id, Relationship)>>>,

    /// All nostr people records currently loaded into memory, keyed by pubkey
    pub people: People,

    /// The relays currently connected to
    pub connected_relays: DashMap<RelayUrl, Vec<RelayJob>>,

    /// The relay picker, used to pick the next relay
    pub relay_picker: RelayPicker<Hooks>,

    /// Whether or not we are shutting down. For the UI (minions will be signaled and
    /// waited for by the overlord)
    pub shutting_down: AtomicBool,

    /// Settings
    pub settings: PRwLock<Settings>,

    /// Signer
    pub signer: Signer,

    /// Dismissed Events
    pub dismissed: RwLock<Vec<Id>>,

    /// Feed
    pub feed: Feed,

    /// Fetcher
    pub fetcher: Fetcher,

    /// Failed Avatars
    /// If in this map, the avatar failed to load or process and is unrecoverable
    /// (but we will take them out and try again if new metadata flows in)
    pub failed_avatars: RwLock<HashSet<PublicKeyHex>>,

    pub pixels_per_point_times_100: AtomicU32,

    /// UI status messages
    pub status_queue: PRwLock<StatusQueue>,

    pub bytes_read: AtomicUsize,

    /// Delegation handling
    pub delegation: Delegation,

    /// Media loading
    pub media: Media,

    /// Search results
    pub people_search_results: PRwLock<Vec<DbPerson>>,
    pub note_search_results: PRwLock<Vec<Event>>,

    /// UI note cache invalidation per note
    // when we update an augment (deletion/reaction/zap) the UI must recompute
    pub ui_notes_to_invalidate: PRwLock<Vec<Id>>,

    /// UI note cache invalidation per person
    // when we update a DbPerson, the UI must recompute all notes by them
    pub ui_people_to_invalidate: PRwLock<Vec<PublicKeyHex>>,

    /// Current zap data, for UI
    pub current_zap: PRwLock<ZapState>,

    /// Hashtag regex
    pub hashtag_regex: Regex,

    /// LMDB storage
    pub storage: Storage,
}

lazy_static! {
    pub static ref GLOBALS: Globals = {

        // Setup a communications channel from the Overlord to the Minions.
        let (to_minions, _) = broadcast::channel(256);

        // Setup a communications channel from the Minions to the Overlord.
        let (to_overlord, tmp_overlord_receiver) = mpsc::unbounded_channel();

        let storage = match Storage::new() {
            Ok(s) => s,
            Err(e) => panic!("{e}")
        };

        Globals {
            first_run: AtomicBool::new(false),
            db: Mutex::new(crate::db::init_database().expect("Failed to setup database connection")),
            to_minions,
            to_overlord,
            tmp_overlord_receiver: Mutex::new(Some(tmp_overlord_receiver)),
            events: Events::new(),
            incoming_events: RwLock::new(Vec::new()),
            relationships: RwLock::new(HashMap::new()),
            people: People::new(),
            connected_relays: DashMap::new(),
            relay_picker: Default::default(),
            shutting_down: AtomicBool::new(false),
            settings: PRwLock::new(Settings::default()),
            signer: Signer::default(),
            dismissed: RwLock::new(Vec::new()),
            feed: Feed::new(),
            fetcher: Fetcher::new(),
            failed_avatars: RwLock::new(HashSet::new()),
            pixels_per_point_times_100: AtomicU32::new(139), // 100 dpi, 1/72th inch => 1.38888
            status_queue: PRwLock::new(StatusQueue::new(
                "Welcome to Gossip. Status messages will appear here. Click them to dismiss them.".to_owned()
            )),
            bytes_read: AtomicUsize::new(0),
            delegation: Delegation::default(),
            media: Media::new(),
            people_search_results: PRwLock::new(Vec::new()),
            note_search_results: PRwLock::new(Vec::new()),
            ui_notes_to_invalidate: PRwLock::new(Vec::new()),
            ui_people_to_invalidate: PRwLock::new(Vec::new()),
            current_zap: PRwLock::new(ZapState::None),
            hashtag_regex: Regex::new(r"(?:^|\W)(#[\w\p{Extended_Pictographic}]+)(?:$|\W)").unwrap(),
            storage,
        }
    };
}

impl Globals {
    pub async fn add_relationship(id: Id, related: Id, relationship: Relationship) {
        let r = (related, relationship);
        let mut relationships = GLOBALS.relationships.write().await;
        relationships
            .entry(id)
            .and_modify(|vec| {
                if !vec.contains(&r) {
                    vec.push(r.clone());
                }
            })
            .or_insert_with(|| vec![r]);
    }

    pub fn get_replies_sync(id: Id) -> Vec<Id> {
        let mut output: Vec<Id> = Vec::new();
        if let Some(vec) = GLOBALS.relationships.blocking_read().get(&id) {
            for (id, relationship) in vec.iter() {
                if *relationship == Relationship::Reply {
                    output.push(*id);
                }
            }
        }

        output
    }

    // FIXME - this allows people to react many times to the same event, and
    //         it counts them all!
    /// Returns the list of reactions and whether or not this account has already reacted to this event
    pub fn get_reactions_sync(id: Id) -> (Vec<(char, usize)>, bool) {
        let mut output: HashMap<char, HashSet<PublicKeyHex>> = HashMap::new();

        // Whether or not the Gossip user already reacted to this event
        let mut self_already_reacted = false;

        if let Some(relationships) = GLOBALS.relationships.blocking_read().get(&id) {
            for (other_id, relationship) in relationships.iter() {
                // get the reacting event to make sure publickeys are unique
                if let Some(e) = GLOBALS.events.get(other_id) {
                    if let Relationship::Reaction(reaction) = relationship {
                        if Some(e.pubkey) == GLOBALS.signer.public_key() {
                            self_already_reacted = true;
                        }

                        let symbol: char = if let Some(ch) = reaction.chars().next() {
                            ch
                        } else {
                            '+'
                        };

                        output
                            .entry(symbol)
                            .and_modify(|pubkeys| {
                                let _ = pubkeys.insert(e.pubkey.into());
                            })
                            .or_insert_with(|| {
                                let mut set = HashSet::new();
                                set.insert(e.pubkey.into());
                                set
                            });
                    }
                }
            }
        }

        let mut v: Vec<(char, usize)> = output.iter().map(|(c, u)| (*c, u.len())).collect();
        v.sort();
        (v, self_already_reacted)
    }

    pub fn get_zap_total_sync(id: Id) -> MilliSatoshi {
        let mut total = MilliSatoshi(0);
        if let Some(relationships) = GLOBALS.relationships.blocking_read().get(&id) {
            for (_other_id, relationship) in relationships.iter() {
                if let Relationship::ZapReceipt(millisats) = relationship {
                    total = total + *millisats;
                }
            }
        }
        total
    }

    pub fn get_deletion_sync(id: Id) -> Option<String> {
        if let Some(relationships) = GLOBALS.relationships.blocking_read().get(&id) {
            for (_id, relationship) in relationships.iter() {
                if let Relationship::Deletion(deletion) = relationship {
                    return Some(deletion.clone());
                }
            }
        }
        None
    }

    pub fn get_your_nprofile() -> Option<Profile> {
        let public_key = match GLOBALS.signer.public_key() {
            Some(pk) => pk,
            None => return None,
        };

        let mut profile = Profile {
            pubkey: public_key,
            relays: Vec::new(),
        };

        match GLOBALS
            .storage
            .filter_relays(|ri| ri.has_usage_bits(DbRelay::OUTBOX))
        {
            Err(e) => {
                tracing::error!("{}", e);
                return None;
            }
            Ok(relays) => {
                for relay in relays {
                    profile.relays.push(relay.url.to_unchecked_url());
                }
            }
        }

        Some(profile)
    }
}
