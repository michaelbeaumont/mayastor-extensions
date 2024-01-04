use crate::{
    common::{
        constants::{
            TWO_DOT_FIVE, TWO_DOT_FOUR, TWO_DOT_ONE, TWO_DOT_O_RC_ONE, TWO_DOT_SIX, TWO_DOT_THREE,
        },
        error::{
            DeserializePromtailExtraConfig, Result, SemverParse,
            SerializePromtailConfigClientToJson, SerializePromtailExtraConfigToJson,
        },
        file::write_to_tempfile,
    },
    helm::{
        chart::{CoreValues, PromtailConfigClient},
        yaml::yq::{YamlKey, YqV4},
    },
};
use semver::Version;
use snafu::ResultExt;
use std::path::Path;
use tempfile::NamedTempFile as TempFile;

/// This compiles all of the helm values options to be passed during the helm chart upgrade.
/// The helm-chart to helm-chart upgrade has two sets of helm-values. They should both be
/// deserialized prior to calling this.
/// Parameters:
///     source_version: &Version --> The helm chart version of the source helm chart. Because the
/// source chart is already installed, this value may be deserialized as a helm releaseElement
/// (https://github.com/helm/helm/blob/v3.13.2/cmd/helm/list.go#L141) member, from a `helm list`
/// output. The 'chart' may then be split into chart-name and chart-version.
///     target_version: &Version --> The helm chart version of the target helm chart. The target
/// helm chart should be available in the local filesystem. The value may be picked out from the
/// Chart.yaml file (version, not appVersion) in the helm chart directory.
///     source_values: &CoreValues --> This is the deserialized struct generated from the helm
/// values of the source chart. The values are sourced from `helm get values --all -o yaml`
///     target_values: &CoreValues --> This is the deserialized struct generated from the
/// locally available target helm chart's values.yaml file.
///     source_values_buf: Vec<u8> --> This is the value that is read from the `helm get values
/// --all -o yaml` output. This is required in a buffer so that it may be written to a file and
/// used with the yq-go binary.
///     target_values_filepath --> This is simply the path to the values.yaml file for the target
/// helm chart, which is available locally.
///     chart_dir --> This is the path to a directory that yq-go may use to write its output file
/// into. The output file will be a merged values.yaml with special values set as per requirement
/// (based on source_version and target_version).
pub(crate) fn generate_values_yaml_file<P, Q>(
    source_version: &Version,
    target_version: &Version,
    source_values: &CoreValues,
    target_values: &CoreValues,
    source_values_buf: Vec<u8>,
    target_values_filepath: P,
    chart_dir: Q,
) -> Result<TempFile>
where
    P: AsRef<Path>,
    Q: AsRef<Path>,
{
    // Write the source_values buffer fetched from the installed helm release to a temporary file.
    let source_values_file: TempFile =
        write_to_tempfile(Some(chart_dir.as_ref()), source_values_buf.as_slice())?;

    // Resultant values yaml for helm upgrade command.
    // Merge the source values with the target values.
    let yq = YqV4::new()?;
    let upgrade_values_yaml =
        yq.merge_files(source_values_file.path(), target_values_filepath.as_ref())?;
    let upgrade_values_file: TempFile =
        write_to_tempfile(Some(chart_dir.as_ref()), upgrade_values_yaml.as_slice())?;

    // Not using semver::VersionReq because expressions like '>=2.1.0' don't include
    // 2.3.0-rc.0. 2.3.0, 2.4.0, etc. are supported. So, not using VersionReq in the
    // below comparisons because of this.

    // Specific special-case values for version 2.0.x.
    let two_dot_o_rc_zero = Version::parse(TWO_DOT_O_RC_ONE).context(SemverParse {
        version_string: TWO_DOT_O_RC_ONE.to_string(),
    })?;
    let two_dot_one = Version::parse(TWO_DOT_ONE).context(SemverParse {
        version_string: TWO_DOT_ONE.to_string(),
    })?;
    if source_version.ge(&two_dot_o_rc_zero) && source_version.lt(&two_dot_one) {
        let log_level_to_replace = "info,io_engine=info";

        if source_values.io_engine_log_level().eq(log_level_to_replace)
            && target_values.io_engine_log_level().ne(log_level_to_replace)
        {
            yq.set_literal_value(
                YamlKey::try_from(".io_engine.logLevel")?,
                target_values.io_engine_log_level(),
                upgrade_values_file.path(),
            )?;
        }
    }

    // Specific special-case values for to-version >=2.1.x.
    if target_version.ge(&two_dot_one) {
        // RepoTags fields will also be set to the values found in the target helm values file
        // (low_priority file). This is so integration tests which use specific repo commits can
        // upgrade to a custom helm chart.
        yq.set_literal_value(
            YamlKey::try_from(".image.repoTags.controlPlane")?,
            target_values.control_plane_repotag(),
            upgrade_values_file.path(),
        )?;
        yq.set_literal_value(
            YamlKey::try_from(".image.repoTags.dataPlane")?,
            target_values.data_plane_repotag(),
            upgrade_values_file.path(),
        )?;
        yq.set_literal_value(
            YamlKey::try_from(".image.repoTags.extensions")?,
            target_values.extensions_repotag(),
            upgrade_values_file.path(),
        )?;
    }

    // Specific special-case values for version 2.3.x.
    let two_dot_three = Version::parse(TWO_DOT_THREE).context(SemverParse {
        version_string: TWO_DOT_THREE.to_string(),
    })?;
    let two_dot_four = Version::parse(TWO_DOT_FOUR).context(SemverParse {
        version_string: TWO_DOT_FOUR.to_string(),
    })?;
    if source_version.ge(&two_dot_three)
        && source_version.lt(&two_dot_four)
        && source_values
            .eventing_enabled()
            .ne(&target_values.eventing_enabled())
    {
        yq.set_literal_value(
            YamlKey::try_from(".eventing.enabled")?,
            target_values.eventing_enabled(),
            upgrade_values_file.path(),
        )?;
    }

    // Special-case values for 2.5.x.
    let two_dot_five = Version::parse(TWO_DOT_FIVE).context(SemverParse {
        version_string: TWO_DOT_FIVE.to_string(),
    })?;
    if source_version.ge(&two_dot_o_rc_zero) && source_version.lt(&two_dot_five) {
        // promtail
        // TODO: check to see if it is the wrong one first.
        if source_values
            .loki_stack_promtail_scrape_configs()
            .ne(target_values.loki_stack_promtail_scrape_configs())
        {
            yq.set_literal_value(
                YamlKey::try_from(".loki-stack.promtail.config.snippets.scrapeConfigs")?,
                target_values.loki_stack_promtail_scrape_configs(),
                upgrade_values_file.path(),
            )?;
        }

        // io_timeout
        let io_timeout_to_replace = "30";
        if source_values
            .csi_node_nvme_io_timeout()
            .eq(io_timeout_to_replace)
            && target_values
                .csi_node_nvme_io_timeout()
                .ne(io_timeout_to_replace)
        {
            yq.set_literal_value(
                YamlKey::try_from(".csi.node.nvme.io_timeout")?,
                target_values.csi_node_nvme_io_timeout(),
                upgrade_values_file.path(),
            )?;
        }
    }

    // Special-case values for 2.6.x.
    let two_dot_six = Version::parse(TWO_DOT_SIX).context(SemverParse {
        version_string: TWO_DOT_SIX.to_string(),
    })?;
    if source_version.ge(&two_dot_o_rc_zero) && source_version.lt(&two_dot_six) {
        // Switch out image tag for the latest one.
        yq.set_object_from_file(
            YamlKey::try_from(".image.tag")?,
            chart_dir
                .as_ref()
                .join("charts/loki-stack/charts/loki/values.yaml"),
            YamlKey::try_from(".loki-stack.loki.image.tag")?,
            upgrade_values_file.path(),
        )?;
        // Delete deprecated objects.
        yq.delete_object(
            YamlKey::try_from(".loki-stack.loki.config.ingester.lifecycler.ring.kvstore")?,
            upgrade_values_file.path(),
        )?;
        yq.delete_object(
            YamlKey::try_from(".loki-stack.promtail.config.snippets.extraClientConfigs")?,
            upgrade_values_file.path(),
        )?;
        yq.delete_object(
            YamlKey::try_from(".loki-stack.promtail.initContainer")?,
            upgrade_values_file.path(),
        )?;

        loki_address_to_clients(source_values, upgrade_values_file.path(), &yq)?;

        let promtail_values_file = chart_dir
            .as_ref()
            .join("charts/loki-stack/charts/promtail/values.yaml");
        yq.set_object_from_file(
            YamlKey::try_from(".config.file")?,
            promtail_values_file.as_path(),
            YamlKey::try_from(".loki-stack.promtail.config.file")?,
            upgrade_values_file.path(),
        )?;
        yq.set_object_from_file(
            YamlKey::try_from(".initContainer")?,
            promtail_values_file.as_path(),
            YamlKey::try_from(".loki-stack.promtail.initContainer")?,
            upgrade_values_file.path(),
        )?;
        yq.set_object_from_file(
            YamlKey::try_from(".readinessProbe.httpGet.path")?,
            promtail_values_file.as_path(),
            YamlKey::try_from(".loki-stack.promtail.readinessProbe.httpGet.path")?,
            upgrade_values_file.path(),
        )?;
    }

    // Default options.
    // Image tag is set because the high_priority file is the user's source options file.
    // The target's image tag needs to be set for PRODUCT upgrade.
    yq.set_literal_value(
        YamlKey::try_from(".image.tag")?,
        target_values.image_tag(),
        upgrade_values_file.path(),
    )?;

    // The CSI sidecar images need to always be the versions set on the chart by default.
    yq.set_literal_value(
        YamlKey::try_from(".csi.image.provisionerTag")?,
        target_values.csi_provisioner_image_tag(),
        upgrade_values_file.path(),
    )?;
    yq.set_literal_value(
        YamlKey::try_from(".csi.image.attacherTag")?,
        target_values.csi_attacher_image_tag(),
        upgrade_values_file.path(),
    )?;
    yq.set_literal_value(
        YamlKey::try_from(".csi.image.snapshotterTag")?,
        target_values.csi_snapshotter_image_tag(),
        upgrade_values_file.path(),
    )?;
    yq.set_literal_value(
        YamlKey::try_from(".csi.image.snapshotControllerTag")?,
        target_values.csi_snapshot_controller_image_tag(),
        upgrade_values_file.path(),
    )?;
    yq.set_literal_value(
        YamlKey::try_from(".csi.image.registrarTag")?,
        target_values.csi_node_driver_registrar_image_tag(),
        upgrade_values_file.path(),
    )?;

    // helm upgrade .. --set image.tag=<version> --set image.repoTags.controlPlane= --set
    // image.repoTags.dataPlane= --set image.repoTags.extensions=

    Ok(upgrade_values_file)
}

/// Converts config.lokiAddress and config.snippets.extraClientConfigs from the promtail helm chart
/// v3.11.0 to config.clients[] which is compatible with promtail helm chart v6.13.1.
fn loki_address_to_clients(
    source_values: &CoreValues,
    upgrade_values_filepath: &Path,
    yq: &YqV4,
) -> Result<()> {
    let promtail_config_clients_yaml_key =
        YamlKey::try_from(".loki-stack.promtail.config.clients")?;
    // Delete existing array, if any. The merge_files() should have added it with the default value
    // set.
    yq.delete_object(
        promtail_config_clients_yaml_key.clone(),
        upgrade_values_filepath,
    )?;
    let loki_address = source_values.loki_stack_promtail_loki_address();
    let promtail_config_client = PromtailConfigClient::with_url(loki_address);
    // Serializing to JSON, because the yq command requires the input in JSON.
    let promtail_config_client = serde_json::to_string(&promtail_config_client).context(
        SerializePromtailConfigClientToJson {
            object: promtail_config_client,
        },
    )?;
    yq.append_to_array(
        promtail_config_clients_yaml_key,
        promtail_config_client,
        upgrade_values_filepath,
    )?;
    // Merge the extraClientConfigs from the promtail v3.11.0 chart to the v6.13.1 chart's
    // config.clients block. Ref: https://github.com/grafana/helm-charts/issues/1214
    // Ref: https://github.com/grafana/helm-charts/pull/1425
    if !source_values.promtail_extra_client_configs().is_empty() {
        // Converting the YAML to a JSON because the yq command requires the object input as a JSON.
        let promtail_extra_client_config: serde_json::Value = serde_yaml::from_str(
            source_values.promtail_extra_client_configs(),
        )
        .context(DeserializePromtailExtraConfig {
            config: source_values.promtail_extra_client_configs().to_string(),
        })?;
        let promtail_extra_client_config = serde_json::to_string(&promtail_extra_client_config)
            .context(SerializePromtailExtraConfigToJson {
                config: promtail_extra_client_config,
            })?;

        yq.append_to_object(
            YamlKey::try_from(".loki-stack.promtail.config.clients[0]")?,
            promtail_extra_client_config,
            upgrade_values_filepath,
        )?;
    }

    // Cleanup config.snippets.extraClientConfig from the promtail chart.
    yq.delete_object(
        YamlKey::try_from(".loki-stack.promtail.config.snippets.extraClientConfigs")?,
        upgrade_values_filepath,
    )?;

    // Cleanup config.lokiAddress from the promtail chart.
    yq.delete_object(
        YamlKey::try_from(".loki-stack.promtail.config.lokiAddress")?,
        upgrade_values_filepath,
    )?;

    Ok(())
}
