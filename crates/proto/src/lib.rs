pub mod agent {
    pub mod v1 {
        tonic::include_proto!("bosun.agent.v1");

        pub const PROTOCOL: &str = "bosun.agent.v1";
    }
}
