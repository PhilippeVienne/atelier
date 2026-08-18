use atelier_common::Workshop;
use kube::CustomResourceExt;

fn main() {
    print!("{}", serde_yaml::to_string(&Workshop::crd()).unwrap());
}
