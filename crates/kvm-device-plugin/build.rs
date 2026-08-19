fn main() {
    tonic_prost_build::configure()
        .build_client(true)
        .build_server(true)
        .compile_protos(&["proto/api.proto"], &["proto"])
        .expect("compilation du proto v1beta1 (kubelet device plugin)");
}
