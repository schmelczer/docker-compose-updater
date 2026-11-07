use docker_compose_updater::compose::parser::ComposeFile;

#[test]
fn image_suffix_survives_yaml_round_trip() {
    let mut file = ComposeFile::parse("services: {web: {image: 'nginx:1.21.0-alpine'}}").unwrap();
    assert_eq!(file.images().unwrap()[0].image_ref.tag, "1.21.0-alpine");
    file.set_image("web", "nginx:1.22.0-alpine").unwrap();
    let yaml = serde_yaml::to_string(&file.document).unwrap();
    let updated = ComposeFile::parse(&yaml).unwrap();
    assert_eq!(updated.images().unwrap()[0].image_ref.tag, "1.22.0-alpine");
}
