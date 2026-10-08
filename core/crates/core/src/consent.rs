//! The user's consent to let the assistant read personal data from the device.
//!
//! What a device tool reads becomes part of the conversation and goes to the
//! model service the user chose, which is not ours. So before the first call
//! of a tool that reads personal data, the person is asked once; the answer is
//! kept and can be changed in Settings. Health data is asked separately.
//!
//! Only the tools that read what is the person's own are gated: calendar,
//! reminders, contacts, location, photos, the clipboard and health. The rest
//! (an alarm, opening a link, weather, language, ...) read nothing of theirs.
//!
//! The system's own permission prompt still stands in front of each framework;
//! this is the first step, and it speaks about where the data goes.

use solos_api::{ConsentKind, Consents as State};
use std::sync::{Arc, RwLock};

/// The consent a tool needs, if it reads the person's own data.
pub fn kind_of_tool(name: &str) -> Option<ConsentKind> {
    match name {
        "device_health" => Some(ConsentKind::Health),
        "device_calendar" | "device_reminders" | "device_contacts" | "device_location" | "device_photos" | "device_clipboard" => {
            Some(ConsentKind::Personal)
        }
        _ => None,
    }
}

type OnChange = Arc<dyn Fn(State) + Send + Sync>;

/// What was answered, and the one question at a time that asks it.
#[derive(Default)]
pub struct Consents {
    state: RwLock<State>,
    /// Two tools called at once must not put two questions on the screen.
    asking: tokio::sync::Mutex<()>,
    on_change: RwLock<Option<OnChange>>,
}

impl Consents {
    pub fn get(&self) -> State {
        self.state.read().unwrap().clone()
    }

    pub fn of(&self, kind: ConsentKind) -> Option<bool> {
        let s = self.state.read().unwrap();
        match kind {
            ConsentKind::Personal => s.personal,
            ConsentKind::Health => s.health,
        }
    }

    /// Replace what is kept, as loaded from the store. Not announced.
    pub fn load(&self, state: State) {
        *self.state.write().unwrap() = state;
    }

    /// Called with the whole state after every change.
    pub fn on_change(&self, f: OnChange) {
        *self.on_change.write().unwrap() = Some(f);
    }

    /// `None` is "not asked yet".
    pub fn set(&self, kind: ConsentKind, answer: Option<bool>) {
        let snapshot = {
            let mut s = self.state.write().unwrap();
            match kind {
                ConsentKind::Personal => s.personal = answer,
                ConsentKind::Health => s.health = answer,
            }
            s.clone()
        };
        if let Some(f) = self.on_change.read().unwrap().clone() {
            f(snapshot);
        }
    }

    /// Whether the tool may read: `Some(answer)` when there is one, `None`
    /// when the person could not be asked just now (the app is not on
    /// screen). An answer already given is used; otherwise `ask` puts the
    /// question (it may block, so it runs off the async threads) and an
    /// answer, yes or no, is kept. Not being able to ask is not an answer
    /// and is not kept.
    pub async fn allowed<F>(&self, kind: ConsentKind, ask: F) -> Option<bool>
    where
        F: FnOnce() -> Option<bool> + Send + 'static,
    {
        if let Some(answer) = self.of(kind) {
            return Some(answer);
        }
        let _one_at_a_time = self.asking.lock().await;
        // Another call may have asked while this one waited.
        if let Some(answer) = self.of(kind) {
            return Some(answer);
        }
        let answer = tokio::task::spawn_blocking(ask).await.ok().flatten();
        if answer.is_some() {
            self.set(kind, answer);
        }
        answer
    }
}

/// The system's own refusal ("Access to calendar is denied; ...") becomes a
/// message that stops the retries: the person said no in the iOS prompt, and
/// the same call will say no again until they change it in iOS Settings. A
/// model told only "denied" tried the same call a dozen times.
pub fn system_refusal(error: &str) -> Option<String> {
    let lower = error.to_lowercase();
    (lower.contains("access to") && (lower.contains("is denied") || lower.contains("restricted"))).then(|| {
        format!(
            "{error} Do not try again: the person refused it in the iOS permission prompt, or it is restricted on this device, and the same call will fail until they allow it in the iOS Settings app (Settings > Solos). Tell them so if it matters to the task."
        )
    })
}

/// What the model is told when the person could not be asked just now.
pub fn unasked(kind: ConsentKind) -> String {
    let what = match kind {
        ConsentKind::Personal => "personal data",
        ConsentKind::Health => "health data",
    };
    format!(
        "The user could not be asked just now (Solos is not on screen), so you may not read their {what} yet. Do not try again. Tell the user you need it and ask them to open Solos and allow it."
    )
}

/// What the model is told when the person said no. It is written for the
/// model: it should not ask again, and should tell the person where to change it.
pub fn refusal(kind: ConsentKind) -> String {
    let what = match kind {
        ConsentKind::Personal => "personal data (calendar, reminders, contacts, location, photos, clipboard)",
        ConsentKind::Health => "health data",
    };
    format!(
        "The user has not allowed you to read their {what}. Do not try again. If it matters to the task, tell the user they can allow it in Solos Settings > Privacy."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn only_tools_that_read_the_persons_own_data_are_gated() {
        for t in ["device_calendar", "device_reminders", "device_contacts", "device_location", "device_photos", "device_clipboard"] {
            assert_eq!(kind_of_tool(t), Some(ConsentKind::Personal), "{t}");
        }
        assert_eq!(kind_of_tool("device_health"), Some(ConsentKind::Health));
        for t in ["device_weather", "device_alarm", "device_open", "device_notify", "device_vision", "device_language", "device_maps", "device_media", "device_info", "shell", "file_read"] {
            assert_eq!(kind_of_tool(t), None, "{t} reads nothing of the person's");
        }
    }

    #[tokio::test]
    async fn the_question_is_asked_once_and_the_answer_is_kept() {
        let c = Consents::default();
        let asked = Arc::new(AtomicUsize::new(0));
        let a = asked.clone();
        assert_eq!(c.allowed(ConsentKind::Personal, move || { a.fetch_add(1, Ordering::SeqCst); Some(true) }).await, Some(true));
        let a = asked.clone();
        assert_eq!(c.allowed(ConsentKind::Personal, move || { a.fetch_add(1, Ordering::SeqCst); Some(false) }).await, Some(true), "already answered");
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        assert_eq!(c.get(), State { personal: Some(true), health: None });
    }

    #[tokio::test]
    async fn a_no_is_kept_too_and_not_asked_again_until_it_is_changed() {
        let c = Consents::default();
        assert_eq!(c.allowed(ConsentKind::Health, || Some(false)).await, Some(false));
        assert_eq!(c.allowed(ConsentKind::Health, || panic!("asked again")).await, Some(false));
        c.set(ConsentKind::Health, Some(true));
        assert_eq!(c.allowed(ConsentKind::Health, || panic!("asked again")).await, Some(true));
        c.set(ConsentKind::Health, None);
        assert_eq!(c.allowed(ConsentKind::Health, || Some(true)).await, Some(true), "not answered: asked again");
    }

    #[tokio::test]
    async fn two_calls_at_once_put_one_question() {
        let c = Arc::new(Consents::default());
        let asked = Arc::new(AtomicUsize::new(0));
        let mut tasks = vec![];
        for _ in 0..4 {
            let (c, asked) = (c.clone(), asked.clone());
            tasks.push(tokio::spawn(async move {
                c.allowed(ConsentKind::Personal, move || {
                    asked.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    Some(true)
                })
                .await
            }));
        }
        for t in tasks {
            assert_eq!(t.await.unwrap(), Some(true));
        }
        assert_eq!(asked.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn not_being_able_to_ask_is_not_an_answer_and_is_not_kept() {
        let c = Consents::default();
        assert_eq!(c.allowed(ConsentKind::Personal, || None).await, None);
        assert_eq!(c.get(), State::default(), "nothing kept");
        assert_eq!(c.allowed(ConsentKind::Personal, || Some(true)).await, Some(true), "asked again later");
    }

    #[test]
    fn a_change_is_announced_with_the_whole_state() {
        let c = Consents::default();
        let seen = Arc::new(std::sync::Mutex::new(vec![]));
        let s = seen.clone();
        c.on_change(Arc::new(move |st| s.lock().unwrap().push(st)));
        c.set(ConsentKind::Personal, Some(true));
        c.set(ConsentKind::Health, Some(false));
        assert_eq!(seen.lock().unwrap().last().unwrap(), &State { personal: Some(true), health: Some(false) });
        c.load(State::default());
        assert_eq!(seen.lock().unwrap().len(), 2, "loading is not a change");
    }

    #[test]
    fn the_systems_own_refusal_tells_the_model_to_stop_and_where_to_allow_it() {
        let r = system_refusal("Access to calendar is denied; it can be allowed in Settings.").unwrap();
        assert!(r.starts_with("Access to calendar is denied") && r.contains("Do not try again") && r.contains("Settings > Solos"));
        assert!(system_refusal("Access to contacts is restricted.").is_some());
        // Other failures keep their own words.
        assert_eq!(system_refusal("Reading reminders timed out."), None);
        assert_eq!(system_refusal("No event with that id."), None);
    }

    #[test]
    fn the_refusal_tells_the_model_not_to_retry_and_where_to_change_it() {
        let r = refusal(ConsentKind::Health);
        assert!(r.contains("health data") && r.contains("Do not try again") && r.contains("Settings > Privacy"));
    }
}
