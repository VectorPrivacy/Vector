//! A relay's NIP-11 name and icon, and whether it asks for AUTH (`relay-info.svelte.js`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use gpui::{App, RenderImage, WeakEntity};

#[derive(Clone, Default)]
pub struct Meta {
    pub name: Option<String>,
    pub icon: Option<Arc<RenderImage>>,
    pub auth: bool,
}

#[derive(Default)]
struct Cache {
    infos: HashMap<String, Option<Meta>>,
    wants: Vec<String>,
    fixture: Option<(HashMap<String, serde_json::Value>, PathBuf)>,
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::default();
}

/// What is known of the relay; the first ask fetches it.
pub fn get(url: &str) -> Option<Meta> {
    CACHE.with_borrow_mut(|c| match c.infos.get(url) {
        Some(m) => m.clone(),
        None => {
            c.infos.insert(url.to_string(), None);
            c.wants.push(url.to_string());
            None
        }
    })
}

pub fn set_fixture(map: HashMap<String, serde_json::Value>, root: PathBuf) {
    CACHE.with_borrow_mut(|c| c.fixture = Some((map, root)));
}

/// Start the fetches the last render asked for; the app repaints as each lands.
pub fn pump<V: 'static>(app: WeakEntity<V>, cx: &mut App) {
    let (wants, fixture) = CACHE.with_borrow_mut(|c| {
        let wants = std::mem::take(&mut c.wants);
        let fixture = c.fixture.as_ref().map(|(map, root)| {
            wants
                .iter()
                .map(|u| (map.get(u).cloned(), root.clone()))
                .collect::<Vec<_>>()
        });
        (wants, fixture)
    });
    for (i, url) in wants.into_iter().enumerate() {
        let app = app.clone();
        let local = fixture.as_ref().map(|f| f[i].clone());
        let rx = local.is_none().then(|| {
            let url = url.clone();
            crate::backend::run(async move {
                let info = vector_core::relay_info::relay_info(&url).await.ok()?;
                let icon = match info.icon.clone() {
                    Some(icon) => crate::backend::inline::image(icon).await,
                    None => None,
                };
                Some((info.name, icon, info.auth))
            })
        });
        cx.spawn(async move |cx| {
            let got = match (rx, local) {
                (Some(rx), _) => rx.await.ok().flatten(),
                (None, Some((Some(v), root))) => {
                    let path = v["icon"].as_str().map(|p| root.join(p));
                    let icon = cx
                        .background_executor()
                        .spawn(async move { path.and_then(|p| std::fs::read(p).ok()) })
                        .await;
                    Some((v["name"].as_str().map(String::from), icon, v["auth"].as_bool() == Some(true)))
                }
                _ => None,
            };
            let Some((name, icon, auth)) = got else { return };
            let icon = match icon {
                Some(b) => {
                    let ext = crate::backend::inline::sniff(&b).unwrap_or("png");
                    cx.background_executor()
                        .spawn(async move { crate::media::decode(&b, ext, (96, 96)) })
                        .await
                        .map(|(i, ..)| Arc::new(i))
                }
                None => None,
            };
            let name = name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
            CACHE.with_borrow_mut(|c| {
                c.infos.insert(url, Some(Meta { name, icon, auth }));
            });
            app.update(cx, |_, cx| cx.notify()).ok();
        })
        .detach();
    }
}
