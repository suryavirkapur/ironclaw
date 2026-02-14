fn main() {
    let proto_file = "proto/ironclaw.proto";

    println!("cargo:rerun-if-changed={proto_file}");

    let mut config = prost_build::Config::new();
    // Enable serde on generated types so we can keep the current websocket JSON surface.
    config.type_attribute(
        ".ironclaw.MessageEnvelope",
        "#[derive(serde::Serialize, serde::Deserialize)]",
    );
    config.type_attribute(
        ".ironclaw.AuthChallenge",
        "#[derive(serde::Serialize, serde::Deserialize)]",
    );
    config.type_attribute(
        ".ironclaw.AuthAck",
        "#[derive(serde::Serialize, serde::Deserialize)]",
    );
    config.type_attribute(
        ".ironclaw.AgentControl",
        "#[derive(serde::Serialize, serde::Deserialize)]",
    );
    config.type_attribute(
        ".ironclaw.AgentState",
        "#[derive(serde::Serialize, serde::Deserialize)]",
    );
    config.type_attribute(
        ".ironclaw.UserMessage",
        "#[derive(serde::Serialize, serde::Deserialize)]",
    );
    config.type_attribute(
        ".ironclaw.ToolCallRequest",
        "#[derive(serde::Serialize, serde::Deserialize)]",
    );
    config.type_attribute(
        ".ironclaw.ToolCallResponse",
        "#[derive(serde::Serialize, serde::Deserialize)]",
    );
    config.type_attribute(
        ".ironclaw.ToolResult",
        "#[derive(serde::Serialize, serde::Deserialize)]",
    );
    config.type_attribute(
        ".ironclaw.StreamDelta",
        "#[derive(serde::Serialize, serde::Deserialize)]",
    );
    config.type_attribute(
        ".ironclaw.JobTrigger",
        "#[derive(serde::Serialize, serde::Deserialize)]",
    );
    config.type_attribute(
        ".ironclaw.JobStatus",
        "#[derive(serde::Serialize, serde::Deserialize)]",
    );
    config.type_attribute(
        ".ironclaw.FileOpRequest",
        "#[derive(serde::Serialize, serde::Deserialize)]",
    );

    config
        .compile_protos(&[proto_file], &["proto"])
        .expect("compile protos");
}
