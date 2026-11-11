//! The lyrics notebook: an op, undo, save/open, old files.

use std::sync::Arc;

use wave_engine::*;

fn page(id: Id, name: &str, start_bar: u32, text: &str) -> LyricPage {
    LyricPage {
        id,
        name: name.into(),
        start_bar,
        text: text.into(),
    }
}

fn notebook() -> Notebook {
    Notebook {
        pages: vec![
            page(
                5000,
                "Zwrotka 1",
                1,
                "[Zwrotka]\nidę przez miasto nocą\n\nświatła mi nie pomogą",
            ),
            page(5001, "Refren", 17, "to jest refren\nłatwy do śpiewania"),
        ],
    }
}

#[test]
fn put_notebook_is_one_undo_step_and_bumps_ids() {
    let e = Engine::new(Project::demo(), Arc::new(NoPlugins));
    assert!(e.project().notebook.pages.is_empty());
    e.apply(
        &[Op::PutNotebook {
            notebook: notebook(),
        }],
        false,
        None,
    )
    .unwrap();
    let p = e.project();
    assert_eq!(p.notebook, notebook());
    assert!(p.next_id > 5001);
    assert!(e.state().dirty);
    let mut edited = notebook();
    edited.pages[1].text.push_str("\nnowa linia");
    e.apply(
        &[Op::PutNotebook {
            notebook: edited.clone(),
        }],
        false,
        None,
    )
    .unwrap();
    assert_eq!(e.project().notebook, edited);
    assert!(e.undo());
    assert_eq!(e.project().notebook, notebook());
    assert!(e.undo());
    assert!(e.project().notebook.pages.is_empty());
    assert!(e.redo());
    assert_eq!(e.project().notebook, notebook());
}

#[test]
fn invalid_notebooks_are_rejected() {
    let e = Engine::new(Project::demo(), Arc::new(NoPlugins));
    let before = e.project();
    let mut dup = notebook();
    dup.pages[1].id = 5000;
    assert!(e
        .apply(&[Op::PutNotebook { notebook: dup }], false, None)
        .is_err());
    let mut zero = notebook();
    zero.pages[0].start_bar = 0;
    assert!(e
        .apply(&[Op::PutNotebook { notebook: zero }], false, None)
        .is_err());
    let mut huge = notebook();
    huge.pages[0].text = "x".repeat(NOTEBOOK_MAX_TEXT + 1);
    assert!(e
        .apply(&[Op::PutNotebook { notebook: huge }], false, None)
        .is_err());
    assert_eq!(e.project(), before);
}

#[test]
fn notebook_survives_save_and_open() {
    let dir = std::env::temp_dir().join(format!("wave-notebook-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("song.wave");
    let e = Engine::new(Project::demo(), Arc::new(NoPlugins));
    e.apply(
        &[Op::PutNotebook {
            notebook: notebook(),
        }],
        false,
        None,
    )
    .unwrap();
    e.save(&path).unwrap();
    let f = Engine::new(Project::empty(), Arc::new(NoPlugins));
    f.open(&path).unwrap();
    assert_eq!(f.project().notebook, notebook());
    assert_eq!(f.project(), e.project());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn old_projects_without_a_notebook_load() {
    let mut v = serde_json::to_value(Project::demo()).unwrap();
    v.as_object_mut().unwrap().remove("notebook");
    let p: Project = serde_json::from_value(v).unwrap();
    assert!(p.notebook.pages.is_empty());
    // a page without startBar/text (hand-written JSON) gets bar 1 and no text
    let pg: LyricPage = serde_json::from_str(r#"{"id":1,"name":"A"}"#).unwrap();
    assert_eq!((pg.start_bar, pg.text.as_str()), (1, ""));
    // camelCase on the wire
    let s = serde_json::to_string(&notebook()).unwrap();
    assert!(s.contains("\"startBar\":17"), "{s}");
}
