use sparkles::Dataset;
use sparkles::builder::VocabCodec;
use sparkles::handles::IndexSetting;
use sparkles::store::IndexSettings;

#[test]
fn embedded_index_settings_share_the_dataset_policy() {
    let temp = tempfile::tempdir().unwrap();
    let dataset = Dataset::open(temp.path()).unwrap();
    dataset.create_branch("work", &Default::default()).unwrap();
    let branch = dataset.branch("work").unwrap();
    let settings: IndexSetting = dataset.settings().index();
    assert_eq!(settings.layer(), IndexSettings::default());
    assert_eq!(settings.get().vocab_codec, Some(VocabCodec::Front));

    let override_settings = IndexSettings {
        vocab_codec: Some(VocabCodec::Fsst),
        patterns: Some(false),
    };
    settings.set(override_settings).unwrap();
    assert_eq!(settings.layer(), override_settings);
    assert_eq!(branch.settings().index().get(), override_settings);
    assert!(matches!(
        branch.settings().index().reset(),
        Err(sparkles::Error::Invalid(_))
    ));
    drop((settings, branch, dataset));

    let dataset = Dataset::open(temp.path()).unwrap();
    let settings = dataset.settings().index();
    assert_eq!(settings.layer(), override_settings);
    settings.reset().unwrap();
    assert_eq!(settings.layer(), IndexSettings::default());
    assert_eq!(settings.get().patterns, Some(true));
}
