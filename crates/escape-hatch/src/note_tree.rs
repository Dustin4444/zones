//! Append-only exit-note Merkle tree.
//!
//! It is implemented as an incremental frontier for cheap root updates
//! and retains leaves (so recovery tooling can build inclusion
//! proofs.)

use alloy_primitives::{B256, keccak256};

/// Maximum supported fixed tree depth.
///
/// At depth=63, capacity is 2^63 which fits in a u64. 2^63 is more leaves than
/// can practically be stored on a single sequencer, so limit it to that for safety.
const MAX_TREE_DEPTH: u8 = 63;

/// Append-only fixed-depth Merkle tree for exit-note commitments.
#[derive(Debug, Clone)]
pub struct ExitNoteTree {
    depth: u8,
    leaf_count: u64,
    branch: Vec<B256>,
    zero_hashes: Vec<B256>,
    leaves: Vec<B256>,
}

impl ExitNoteTree {
    /// Create an empty tree with `2^depth` leaf capacity.
    pub fn new(depth: u8) -> Result<Self, ExitNoteTreeError> {
        if depth > MAX_TREE_DEPTH {
            return Err(ExitNoteTreeError::DepthTooLarge {
                depth,
                max: MAX_TREE_DEPTH,
            });
        }

        let zero_hashes = build_zero_hashes(depth);

        Ok(Self {
            depth,
            leaf_count: 0,
            branch: vec![B256::ZERO; depth as usize],
            zero_hashes,
            leaves: Vec::new(),
        })
    }

    /// Fixed tree depth.
    pub const fn depth(&self) -> u8 {
        self.depth
    }

    /// Number of appended leaves in the tree.
    pub const fn len(&self) -> u64 {
        self.leaf_count
    }

    pub const fn is_empty(&self) -> bool {
        self.leaf_count == 0
    }

    pub const fn capacity(&self) -> u64 {
        1u64 << self.depth
    }

    /// Append one exit-note commitment and return its leaf index.
    pub fn append(&mut self, commitment: B256) -> Result<u64, ExitNoteTreeError> {
        if self.leaf_count == self.capacity() {
            return Err(ExitNoteTreeError::TreeFull {
                depth: self.depth,
                capacity: self.capacity(),
            });
        }

        let index = self.leaf_count;
        let mut node = commitment;
        let mut size = self.leaf_count;

        for level in 0..self.depth as usize {
            if size & 1 == 0 {
                self.branch[level] = node;
                break;
            }

            node = hash_pair(self.branch[level], node);
            size >>= 1;
        }

        self.leaf_count += 1;
        self.leaves.push(commitment);

        Ok(index)
    }

    /// Append a batch of exit-note commitments and return the appended range.
    pub fn append_many<I>(
        &mut self,
        commitments: I,
    ) -> Result<Option<AppendRange>, ExitNoteTreeError>
    where
        I: IntoIterator<Item = B256>,
    {
        let mut first_index = None;
        let mut last_index = None;
        let mut count = 0;

        for commitment in commitments {
            let index = self.append(commitment)?;
            first_index.get_or_insert(index);
            last_index = Some(index);
            count += 1;
        }

        Ok(first_index.map(|first_index| AppendRange {
            first_index,
            last_index: last_index.expect("last index exists when first index exists"),
            count,
        }))
    }

    /// Return the current padded Merkle root.
    pub fn root(&self) -> B256 {
        root_from_frontier(self.depth, self.leaf_count, &self.branch, &self.zero_hashes)
    }

    /// Return an appended leaf by index.
    pub fn leaf(&self, index: u64) -> Option<B256> {
        self.leaves.get(index as usize).copied()
    }

    /// Build an inclusion proof for an appended leaf.
    pub fn proof(&self, index: u64) -> Result<ExitNoteTreeProof, ExitNoteTreeError> {
        let commitment = self
            .leaf(index)
            .ok_or(ExitNoteTreeError::LeafIndexOutOfBounds {
                index,
                leaf_count: self.leaf_count,
            })?;

        let siblings = proof_siblings(self.depth, &self.zero_hashes, &self.leaves, index);

        Ok(ExitNoteTreeProof {
            index,
            commitment,
            siblings,
        })
    }
}

impl Default for ExitNoteTree {
    fn default() -> Self {
        Self::new(32).expect("default exit-note tree depth is valid")
    }
}

/// Contiguous leaf-index range appended by one batch operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppendRange {
    /// First appended leaf index.
    pub first_index: u64,
    /// Last appended leaf index.
    pub last_index: u64,
    /// Number of appended leaves.
    pub count: u64,
}

/// Merkle inclusion proof for one exit-note commitment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitNoteTreeProof {
    /// Leaf index of the note commitment.
    pub index: u64,
    /// Note commitment stored at `index`.
    pub commitment: B256,
    /// Sibling hashes from leaf level to root level.
    pub siblings: Vec<B256>,
}

impl ExitNoteTreeProof {
    /// Verify this proof against `root` for a tree of `depth`.
    pub fn verify(&self, root: B256, depth: u8) -> bool {
        if self.siblings.len() != depth as usize {
            return false;
        }

        if depth > MAX_TREE_DEPTH || self.index >= (1u64 << depth) {
            return false;
        }

        let mut node = self.commitment;
        let mut index = self.index;

        for sibling in &self.siblings {
            node = if index & 1 == 0 {
                hash_pair(node, *sibling)
            } else {
                hash_pair(*sibling, node)
            };
            index >>= 1;
        }

        node == root
    }
}

/// Exit-note tree errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitNoteTreeError {
    /// Requested tree depth is not supported.
    DepthTooLarge {
        /// Requested depth.
        depth: u8,
        /// Maximum supported depth.
        max: u8,
    },
    /// The fixed-size tree has no remaining leaf slots.
    TreeFull {
        /// Fixed tree depth.
        depth: u8,
        /// Maximum leaf count.
        capacity: u64,
    },
    /// Requested proof/leaf index has not been appended.
    LeafIndexOutOfBounds {
        /// Requested leaf index.
        index: u64,
        /// Number of appended leaves.
        leaf_count: u64,
    },
}

impl std::fmt::Display for ExitNoteTreeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DepthTooLarge { depth, max } => {
                write!(f, "exit-note tree depth {depth} exceeds maximum {max}")
            }
            Self::TreeFull { depth, capacity } => {
                write!(
                    f,
                    "exit-note tree of depth {depth} is full at capacity {capacity}"
                )
            }
            Self::LeafIndexOutOfBounds { index, leaf_count } => {
                write!(
                    f,
                    "exit-note leaf index {index} is out of bounds for {leaf_count} leaves"
                )
            }
        }
    }
}

impl std::error::Error for ExitNoteTreeError {}

fn build_zero_hashes(depth: u8) -> Vec<B256> {
    let mut zero_hashes = Vec::with_capacity(depth as usize + 1);
    zero_hashes.push(B256::ZERO);

    for level in 0..depth as usize {
        let zero = zero_hashes[level];
        zero_hashes.push(hash_pair(zero, zero));
    }

    zero_hashes
}

fn root_from_frontier(depth: u8, leaf_count: u64, branch: &[B256], zero_hashes: &[B256]) -> B256 {
    let mut node = B256::ZERO;

    for level in 0..depth as usize {
        node = if (leaf_count >> level) & 1 == 1 {
            hash_pair(branch[level], node)
        } else {
            hash_pair(node, zero_hashes[level])
        };
    }

    node
}

fn proof_siblings(depth: u8, zero_hashes: &[B256], leaves: &[B256], index: u64) -> Vec<B256> {
    let mut siblings = Vec::with_capacity(depth as usize);
    let mut level_nodes = leaves.to_vec();
    let mut index = index as usize;

    for zero_hash in zero_hashes.iter().take(depth as usize) {
        let sibling_index = index ^ 1;
        let sibling = level_nodes
            .get(sibling_index)
            .copied()
            .unwrap_or(*zero_hash);
        siblings.push(sibling);

        let next_len = level_nodes.len().div_ceil(2);
        let mut next_level = Vec::with_capacity(next_len);

        for pair_index in 0..next_len {
            let left = level_nodes
                .get(pair_index * 2)
                .copied()
                .unwrap_or(*zero_hash);
            let right = level_nodes
                .get(pair_index * 2 + 1)
                .copied()
                .unwrap_or(*zero_hash);
            next_level.push(hash_pair(left, right));
        }

        level_nodes = next_level;
        index >>= 1;
    }

    siblings
}

fn hash_pair(left: B256, right: B256) -> B256 {
    let mut preimage = [0u8; 64];
    preimage[..32].copy_from_slice(left.as_slice());
    preimage[32..].copy_from_slice(right.as_slice());
    keccak256(preimage)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(byte: u8) -> B256 {
        B256::repeat_byte(byte)
    }

    fn full_root(depth: u8, leaves: &[B256]) -> B256 {
        let zero_hashes = build_zero_hashes(depth);
        let capacity = 1usize << depth;
        let mut level_nodes = Vec::with_capacity(capacity);
        level_nodes.extend_from_slice(leaves);
        level_nodes.resize(capacity, B256::ZERO);

        for zero_hash in zero_hashes.iter().take(depth as usize) {
            level_nodes = level_nodes
                .chunks(2)
                .map(|pair| {
                    let left = pair[0];
                    let right = pair.get(1).copied().unwrap_or(*zero_hash);
                    hash_pair(left, right)
                })
                .collect();
        }

        level_nodes[0]
    }

    #[test]
    fn hash_pair_is_keccak256_left_concat_right() {
        let left = leaf(0x11);
        let right = leaf(0x22);
        let mut preimage = [0u8; 64];
        preimage[..32].copy_from_slice(left.as_slice());
        preimage[32..].copy_from_slice(right.as_slice());

        assert_eq!(hash_pair(left, right), keccak256(preimage));
    }

    #[test]
    fn empty_leaf_value_is_zero() {
        let zero_hashes = build_zero_hashes(1);

        assert_eq!(zero_hashes[0], B256::ZERO);
    }

    #[test]
    fn empty_root_matches_padded_empty_tree() {
        let tree = ExitNoteTree::new(3).unwrap();

        assert_eq!(tree.len(), 0);
        assert!(tree.is_empty());
        assert_eq!(tree.capacity(), 8);
        assert_eq!(tree.root(), full_root(3, &[]));
    }

    #[test]
    fn append_returns_monotonic_indices_and_updates_root() {
        let mut tree = ExitNoteTree::new(3).unwrap();
        let leaves = [leaf(1), leaf(2), leaf(3), leaf(4), leaf(5)];

        for (expected_index, commitment) in leaves.iter().copied().enumerate() {
            let index = tree.append(commitment).unwrap();

            assert_eq!(index, expected_index as u64);
            assert_eq!(tree.len(), expected_index as u64 + 1);
            assert_eq!(tree.leaf(index), Some(commitment));
            assert_eq!(tree.root(), full_root(3, &leaves[..=expected_index]));
        }
    }

    #[test]
    fn append_many_returns_first_index() {
        let mut tree = ExitNoteTree::new(3).unwrap();

        assert_eq!(tree.append_many([]).unwrap(), None);
        assert_eq!(
            tree.append_many([leaf(1), leaf(2)]).unwrap(),
            Some(AppendRange {
                first_index: 0,
                last_index: 1,
                count: 2,
            })
        );
        assert_eq!(
            tree.append_many([leaf(3), leaf(4)]).unwrap(),
            Some(AppendRange {
                first_index: 2,
                last_index: 3,
                count: 2,
            })
        );
        assert_eq!(tree.len(), 4);
    }

    #[test]
    fn inclusion_proofs_verify_for_each_appended_leaf() {
        let mut tree = ExitNoteTree::new(3).unwrap();
        let leaves = [leaf(1), leaf(2), leaf(3), leaf(4), leaf(5)];

        for commitment in leaves {
            tree.append(commitment).unwrap();
        }

        let root = tree.root();
        for index in 0..tree.len() {
            let proof = tree.proof(index).unwrap();

            assert_eq!(proof.index, index);
            assert_eq!(proof.commitment, tree.leaf(index).unwrap());
            assert_eq!(proof.siblings.len(), tree.depth() as usize);
            assert!(proof.verify(root, tree.depth()));
        }
    }

    #[test]
    fn proof_rejects_wrong_root_or_depth() {
        let mut tree = ExitNoteTree::new(3).unwrap();
        tree.append(leaf(1)).unwrap();

        let proof = tree.proof(0).unwrap();

        assert!(!proof.verify(leaf(9), tree.depth()));
        assert!(!proof.verify(tree.root(), tree.depth() + 1));
    }

    #[test]
    fn rejects_out_of_bounds_proof_index() {
        let mut tree = ExitNoteTree::new(3).unwrap();
        tree.append(leaf(1)).unwrap();

        assert_eq!(
            tree.proof(1),
            Err(ExitNoteTreeError::LeafIndexOutOfBounds {
                index: 1,
                leaf_count: 1,
            })
        );
    }

    #[test]
    fn rejects_appends_after_tree_is_full() {
        let mut tree = ExitNoteTree::new(1).unwrap();
        tree.append(leaf(1)).unwrap();
        tree.append(leaf(2)).unwrap();

        assert_eq!(
            tree.append(leaf(3)),
            Err(ExitNoteTreeError::TreeFull {
                depth: 1,
                capacity: 2,
            })
        );
    }

    #[test]
    fn rejects_unsupported_depth() {
        let err = ExitNoteTree::new(MAX_TREE_DEPTH + 1).unwrap_err();

        assert_eq!(
            err,
            ExitNoteTreeError::DepthTooLarge {
                depth: MAX_TREE_DEPTH + 1,
                max: MAX_TREE_DEPTH,
            }
        );
    }
}
