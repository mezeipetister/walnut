use walnut::Group;

fn test_address(group_index: u32, bitmap_index: u32) {
    // println!(
    //     "initial => group_index: {}, bitmap_index: {}",
    //     group_index, bitmap_index
    // );
    let block_index = Group::create_public_address(group_index, bitmap_index);
    // println!("computed => block_index: {}", block_index);
    let (group_index2, bitmap_index2) = Group::translate_public_address(block_index);
    // println!(
    //     "translated => group_index: {}, bitmap_index: {}",
    //     group_index2, bitmap_index2
    // );
    // println!("-----------------------")
    assert_eq!(group_index, group_index2);
    assert_eq!(bitmap_index, bitmap_index2);
}

fn main() {
    for group_index in 0..20 {
        for bitmap_index in 0..32_768 {
            // println!("test {} -> {}", group_index, bitmap_index);
            test_address(group_index, bitmap_index);
        }
    }
    println!("Ok");
}
