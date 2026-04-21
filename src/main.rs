use porchetta::store::PorchettaStore;

fn main() {
    let store = PorchettaStore::init().unwrap();
    println!("{:#?}", store);
    println!("{:#?}", store.read_manifest().unwrap());
}
