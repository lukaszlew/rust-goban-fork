//! Module with the goban and his implementations.

use std::fmt::Display;
use std::fmt::Error;
use std::fmt::Formatter;
use std::hash::{Hash, Hasher};

use arrayvec::ArrayVec;

use crate::one2dim;
use crate::pieces::chain::{Chain, Liberties, merge, set};
use crate::pieces::Nat;
use crate::pieces::stones::*;
use crate::pieces::util::CircularRenIter;
use crate::pieces::util::coord::{
    Coord, is_coord_valid, neighbor_coords, one_to_2dim, Size, two_to_1dim,
};
use crate::pieces::zobrist::*;

pub type ChainIdx = usize;
pub type BoardIdx = usize;

const BOARD_MAX_SIZE: (Nat, Nat) = (19, 19);
const BOARD_MAX_LENGTH: usize = BOARD_MAX_SIZE.0 as usize * BOARD_MAX_SIZE.1 as usize;
const MAX_CHAINS: usize = 4 * BOARD_MAX_LENGTH / 5;

macro_rules! iter_stones {
    ($goban: expr, $ren_idx: expr) => {
        CircularRenIter::new(
            $goban.chains[$ren_idx as usize].origin as usize,
            &$goban.next_stone,
        )
    };
}

/// Represents a goban. the stones are stored in ROW MAJOR (row, column)
#[derive(Debug, Clone)]
pub struct Goban {
    pub(super) chains: Vec<Chain>,
    //free_slots: BitArr!(for MAX_CHAINS),
    board: Vec<Option<u16>>,
    next_stone: Vec<u16>,
    size: Size,
    zobrist_hash: u64,
}

impl Goban {
    /// Creates a Goban
    /// # Arguments
    ///
    /// * `(height, width)` a tuple with the height and the width of the desired goban.
    pub fn new((height, width): Size) -> Self {
        assert!(height <= 19 && width <= 19,);
        Goban {
            size: (height, width),
            zobrist_hash: 0,
            board: vec![None; BOARD_MAX_LENGTH],
            next_stone: vec![0; BOARD_MAX_LENGTH],
            chains: Vec::with_capacity(MAX_CHAINS),
            //free_slots: Default::default(),
        }
    }

    /// Creates a Goban from an array of stones.
    pub fn from_array(stones: &[MaybeColor]) -> Self {
        let size = ((stones.len() as f32).sqrt()) as u8;
        let mut game = Goban::new((size, size));
        stones
            .iter()
            .enumerate()
            .map(|(index, color)| (one_to_2dim((size, size), index), color))
            .filter_map(|(coord, mcolor)| mcolor.map(|color| (coord, color)))
            .for_each(|coord_color| {
                game.push(coord_color.0, coord_color.1);
            });
        game
    }

    pub fn size(&self) -> Size {
        self.size
    }

    pub fn zobrist_hash(&self) -> u64 {
        self.zobrist_hash
    }

    /// Returns the underlying goban in a vector with a RowMajor Policy, calculated on the fly.
    pub fn to_vec(&self) -> Vec<MaybeColor> {
        self.board
            .iter()
            .map(|point| {
                point.map_or(EMPTY, |go_str_ptr| {
                    self.chains[go_str_ptr as usize].color.into()
                })
            })
            .collect()
    }

    /// Like vec but in a matrix shape.
    pub fn matrix(&self) -> Vec<Vec<MaybeColor>> {
        let mut mat = vec![vec![]];
        for line in self.board.chunks_exact(self.size.1 as usize) {
            let v = line
                .iter()
                .map(|o| o.map_or(EMPTY, |idx| self.chains[idx as usize].color.into()))
                .collect();
            mat.push(v);
        }
        mat
    }

    /// Get number of stones on the goban.
    /// (number of black stones, number of white stones)
    pub fn number_of_stones(&self) -> (u32, u32) {
        self.get_stones()
            .fold((0, 0), |(x1, x2), stone| match stone.color {
                Color::Black => (x1 + 1, x2),
                Color::White => (x1, x2 + 1),
            })
    }

    #[inline]
    pub(crate) fn board(&self) -> &[Option<u16>] {
        &self.board
    }

    /// pushes the stone
    /// # Arguments
    /// point: the point where the stone will be placed
    /// color: the color of the stone must be != empty
    /// # Returns
    /// A tuple with (the ren without liberties, the ren where the point was added)
    pub(crate) fn push_wth_feedback(
        &mut self,
        point: Coord,
        color: Color,
    ) -> (ArrayVec<usize, 4>, ChainIdx) {
        let pushed_stone_idx = two_to_1dim(self.size, point);

        let mut adjacent_same_color_str_set = ArrayVec::<BoardIdx, 4>::new();
        let mut adjacent_opposite_color_str_set = ArrayVec::<BoardIdx, 4>::new();
        let mut liberties = ArrayVec::<BoardIdx, 4>::new();

        for neighbor_idx in self.neighbors_idx(pushed_stone_idx) {
            match self.board[neighbor_idx] {
                Some(adj_ren_index) => {
                    let adj_ren_index = adj_ren_index as usize;
                    if self.chains[adj_ren_index].color == color {
                        if !adjacent_same_color_str_set.contains(&adj_ren_index) {
                            adjacent_same_color_str_set.push(adj_ren_index);
                        }
                    } else if !adjacent_opposite_color_str_set.contains(&adj_ren_index) {
                        adjacent_opposite_color_str_set.push(adj_ren_index);
                    }
                }
                None => {
                    liberties.push(neighbor_idx);
                }
            }
        }
        let mut dead_ren = ArrayVec::<BoardIdx, 4>::new();
        // for every string of opposite color remove a liberty and update the string.
        for ren_idx in adjacent_opposite_color_str_set {
            let ren = &mut self.chains[ren_idx];
            if ren.used {
                ren.remove_liberty(pushed_stone_idx);
                if ren.is_dead() {
                    dead_ren.push(ren_idx);
                }
            }
        }

        let number_of_neighbors_strings = adjacent_same_color_str_set.len();
        let updated_ren_index = match number_of_neighbors_strings {
            0 => self.create_chain(pushed_stone_idx, color, &liberties),
            1 => {
                let only_ren_idx = adjacent_same_color_str_set[0];

                self.chains[only_ren_idx]
                    .remove_liberty(pushed_stone_idx)
                    .union_liberties_slice(&liberties);
                self.add_stone_to_chain(only_ren_idx, pushed_stone_idx);
                self.board[pushed_stone_idx] = Some(only_ren_idx as u16);
                only_ren_idx
            }
            _ => {
                let mut to_merge = self.create_chain(pushed_stone_idx, color, &liberties);
                for adj_ren in adjacent_same_color_str_set {
                    if self.chains[adj_ren].number_of_liberties()
                        < self.chains[to_merge].number_of_liberties()
                    {
                        self.merge_strings(to_merge, adj_ren);
                    } else {
                        self.merge_strings(adj_ren, to_merge);
                        to_merge = adj_ren;
                    }
                }
                self.chains[to_merge].remove_liberty(pushed_stone_idx);
                to_merge
            }
        };
        self.zobrist_hash ^= index_zobrist(pushed_stone_idx, color);
        #[cfg(debug_assertions)]
        self.check_integrity_all();
        (dead_ren, updated_ren_index)
    }

    pub(crate) fn remove_captured_stones_aux(
        &mut self,
        color: Color,
        suicide_allowed: bool,
        prisoners: (u32, u32),
        dead_rens_indices: &[ChainIdx],
        added_ren: ChainIdx,
    ) -> ((u32, u32), Option<Coord>) {
        let only_one_ren_removed = dead_rens_indices.len() == 1;
        let mut stones_removed = prisoners;
        let mut ko_point = None;
        for &dead_ren_idx in dead_rens_indices {
            let dead_ren = &self.chains[dead_ren_idx];
            if dead_ren.num_stones == 1 && only_one_ren_removed {
                ko_point = Some(one_to_2dim(self.size(), dead_ren.origin as usize));
            }
            stones_removed = match color {
                Color::White => (
                    stones_removed.0,
                    stones_removed.1 + dead_ren.num_stones as u32,
                ),
                Color::Black => (
                    stones_removed.0 + dead_ren.num_stones as u32,
                    stones_removed.1,
                ),
            };
            self.remove_chain(dead_ren_idx);
        }
        let &mut Chain { num_stones, .. } = &mut self.chains[added_ren];
        if suicide_allowed && num_stones == 0 {
            self.remove_chain(added_ren);
            ko_point = None;
            let num_stones = num_stones as u32;
            stones_removed = match color {
                Color::Black => (stones_removed.0, stones_removed.1 + num_stones),
                Color::White => (stones_removed.0 + num_stones, stones_removed.1),
            };
        }
        (stones_removed, ko_point)
    }

    /// Put a stones in the goban.
    /// default (line, column)
    /// the (0,0) point is in the top left.
    ///
    /// # Panics
    /// if the point is out of bounds
    pub fn push(&mut self, point: Coord, color: Color) -> &mut Self {
        debug_assert!(
            (point.0) < self.size.0,
            "Coordinate point.0 {} out of bounds",
            point.0
        );
        debug_assert!(
            (point.1) < self.size.1,
            "Coordinate point.1 {} out of bounds",
            point.1
        );
        self.push_wth_feedback(point, color);
        self
    }

    /// Helper function to put a stone.
    #[inline]
    pub fn push_stone(&mut self, stone: Stone) -> &mut Goban {
        self.push(stone.coord, stone.color)
    }

    /// Put many stones.
    #[inline]
    pub fn push_many(&mut self, points: &[Coord], value: Color) {
        points.iter().for_each(|&point| {
            self.push(point, value);
        })
    }

    pub fn get_chain_by_board_idx(&self, board_idx: BoardIdx) -> Option<&Chain> {
        self.board[board_idx].map(|chain| &self.chains[chain as usize])
    }

    pub fn get_chain_by_point(&self, point: Coord) -> Option<&Chain> {
        self.get_chain_by_board_idx(two_to_1dim(self.size, point))
    }

    /// Get all the neighbors to the coordinate including empty intersections.
    #[inline]
    pub fn get_neighbors_points(&self, point: Coord) -> impl Iterator<Item=Point> + '_ {
        self.neighbors_coords(point).map(move |p| Point {
            coord: p,
            color: self.get_color(p),
        })
    }

    /// Get all the stones that are neighbor to the coord except empty intersections.
    #[inline]
    pub fn get_neighbors_stones(&self, point: Coord) -> impl Iterator<Item=Stone> + '_ {
        self.get_neighbors_points(point).filter_map(|x| {
            if x.is_empty() {
                None
            } else {
                Some(x.into())
            }
        })
    }

    /// Get all the neighbors indexes to the point. Only return point with a color.
    #[inline]
    pub fn get_neighbors_chain_indexes(&self, coord: Coord) -> impl Iterator<Item=ChainIdx> + '_ {
        self.neighbors_coords(coord)
            .map(move |point| two_to_1dim(self.size, point))
            .filter_map(move |point| self.board[point].map(|idx| idx as usize))
    }

    /// Get all the chains adjacent to the point. The result iterator can contains duplicates.
    #[inline]
    pub fn get_neighbors_chains(&self, coord: Coord) -> impl Iterator<Item=&Chain> + '_ {
        self.get_neighbors_chain_indexes(coord)
            .map(move |chain_idx| &self.chains[chain_idx])
    }

    #[inline]
    pub fn get_neighbors_chains_ids_by_board_idx(
        &self,
        index: BoardIdx,
    ) -> impl Iterator<Item=ChainIdx> + '_ {
        self.neighbors_idx(index)
            .filter_map(move |idx| self.board[idx].map(|idx| idx as usize))
    }

    /// Function for getting the stone in the goban.
    #[inline(always)]
    pub fn get_point(&self, coord: Coord) -> Point {
        Point {
            coord,
            color: self.board[two_to_1dim(self.size, coord)]
                .map(|chain_idx| self.chains[chain_idx as usize].color),
        }
    }

    pub fn get_color(&self, coord: Coord) -> MaybeColor {
        self.board[two_to_1dim(self.size, coord)]
            .map(|chain_id| self.chains[chain_id as usize].color)
    }

    pub fn get_stone_color(&self, coord: Coord) -> Color {
        self.get_color(coord).expect("Tried to unwrap a point")
    }

    /// Get all the stones except "EMPTY stones"
    #[inline]
    pub fn get_stones(&self) -> impl Iterator<Item=Stone> + '_ {
        self.board.iter().enumerate().filter_map(move |(index, o)| {
            o.map(move |chain_idx| Stone {
                coord: one_to_2dim(self.size, index),
                color: self.chains[chain_idx as usize].color,
            })
        })
    }

    /// Get stones by their color.
    #[inline]
    pub fn get_stones_by_color(&self, color: MaybeColor) -> impl Iterator<Item=Point> + '_ {
        self.get_coords_by_color(color)
            .map(move |c| Point { color, coord: c })
    }

    pub fn get_empty_idx(&self) -> impl Iterator<Item=BoardIdx> + '_ {
        self.board
            .iter()
            .enumerate()
            .filter_map(|(idx, chain)| chain.map(|_| idx))
    }

    pub fn get_empty_coords(&self) -> impl Iterator<Item=Coord> + '_ {
        let board_length = self.size.0 as usize * self.size.1 as usize;
        self.board[..board_length]
            .iter()
            .enumerate()
            .filter_map(|x| {
                if x.1.is_none() {
                    Some(one2dim!(self.size, x.0))
                } else {
                    None
                }
            })
    }

    /// Get points by their color.
    #[inline]
    pub fn get_coords_by_color(&self, color: MaybeColor) -> impl Iterator<Item=Coord> + '_ {
        let mut res = ArrayVec::<Coord, BOARD_MAX_LENGTH>::new();
        for board_idx in 0..(self.size.0 * self.size.1) as usize {
            match color {
                EMPTY => res.push(one_to_2dim(self.size, board_idx)),
                Some(c) => self.board[board_idx]
                    .filter(|&chain_idx| self.chains[chain_idx as usize].color == c)
                    .map(|_| res.push(one_to_2dim(self.size, board_idx)))
                    .unwrap_or(()),
            }
        }
        res.into_iter()
    }

    /// Returns the "empty" stones connected to the stone
    #[inline]
    pub fn get_liberties(&self, coord: Coord) -> impl Iterator<Item=Coord> + '_ {
        self.neighbors_coords(coord).filter(|&x| self.get_color(x).is_none())
    }

    /// Returns true if the stone has liberties.
    #[inline]
    pub fn has_liberties(&self, coord: Coord) -> bool {
        self.get_liberties(coord).next().is_some()
    }

    /// Get a string for printing the goban in normal shape (0,0) left bottom
    pub fn pretty_string(&self) -> String {
        let mut buff = String::with_capacity(361);
        for i in 0..self.size.0 as Nat {
            for j in 0..self.size.1 as Nat {
                buff.push(match self.get_color((i, j)) {
                    Some(Color::Black) => '●',
                    Some(Color::White) => '○',
                    EMPTY => {
                        match (
                            i == 0,
                            i == self.size.0 as Nat - 1,
                            j == 0,
                            j == self.size.1 as Nat - 1,
                        ) {
                            (true, _, true, _) => '┏',
                            (true, _, _, true) => '┓',

                            (_, true, true, _) => '┗',
                            (_, true, _, true) => '┛',

                            (true, _, _, _) => '┯',
                            (_, true, _, _) => '┷',
                            (_, _, true, _) => '┠',
                            (_, _, _, true) => '┨',
                            _ => '┼',
                        }
                    }
                });
            }
            buff.push('\n');
        }
        buff
    }

    /// Remove a string from the game, it add liberties to all
    /// adjacent chains that aren't the same color.
    pub fn remove_chain(&mut self, ren_to_remove_idx: ChainIdx) {
        let color_of_the_string = self.chains[ren_to_remove_idx].color;
        let mut neighbors = ArrayVec::<BoardIdx, 4>::new();

        for point_idx in iter_stones!(self, ren_to_remove_idx as u16) {
            for neighbor_str_idx in self.get_neighbors_chains_ids_by_board_idx(point_idx) {
                if ren_to_remove_idx != neighbor_str_idx {
                    #[cfg(debug_assertions)]
                    if !neighbors.contains(&neighbor_str_idx) {
                        neighbors.push(neighbor_str_idx)
                    }
                    #[cfg(not(debug_assertions))]
                    neighbors.push(neighbor_str_idx)
                }
            }

            for &n in &neighbors {
                self.chains[n].add_liberty(point_idx);
            }
            neighbors.clear();
            self.zobrist_hash ^= index_zobrist(point_idx, color_of_the_string);
            self.board[point_idx] = None;
        }
        self.put_chain_in_bin(ren_to_remove_idx);
    }

    /// Updates the indexes to match actual goban. must use after we put a stone.
    fn update_chain_indexes_in_board(&mut self, ren_idx: ChainIdx) {
        debug_assert_eq!(
            iter_stones!(self, ren_idx).last().unwrap() as u16,
            self.chains[ren_idx].last
        );
        for point in iter_stones!(self, ren_idx) {
            unsafe {
                *self.board.get_unchecked_mut(point) = Some(ren_idx as u16);
            }
        }
    }

    /// Get the neighbors points of a point.
    #[inline]
    fn neighbors_coords(&self, coord: Coord) -> impl Iterator<Item=Coord> {
        let size = self.size;
        neighbor_coords(coord)
            .into_iter()
            .filter(move |&p| is_coord_valid(size, p))
    }

    #[inline]
    fn neighbors_idx(&self, board_idx: BoardIdx) -> impl Iterator<Item=BoardIdx> {
        let size = self.size;
        self.neighbors_coords(one_to_2dim(size, board_idx))
            .filter(move |&coord| is_coord_valid(size, coord))
            .map(move |coord| two_to_1dim(size, coord))
    }

    pub fn get_chain_it(&self, chain_idx: ChainIdx) -> impl Iterator<Item=BoardIdx> + '_ {
        CircularRenIter::new(self.chains[chain_idx].origin as usize, &self.next_stone)
    }

    #[inline]
    pub fn get_chain_it_by_board_idx(
        &self,
        board_idx: BoardIdx,
    ) -> impl Iterator<Item=BoardIdx> + '_ {
        self.board[board_idx]
            .map(|chain_idx| self.get_chain_it(chain_idx as ChainIdx))
            .unwrap_or_else(|| panic!("The board index: {board_idx} was out of bounds"))
    }

    #[inline]
    fn create_chain(&mut self, origin: BoardIdx, color: Color, liberties: &[BoardIdx]) -> ChainIdx {
        let mut lib_bitvec: Liberties = Default::default();
        for &board_idx in liberties {
            set::<true>(board_idx, &mut lib_bitvec);
        }
        let chain_to_place = Chain::new_with_liberties(color, origin, lib_bitvec);
        self.next_stone[origin] = origin as u16;
        self.chains.push(chain_to_place);
        let chain_idx = self.chains.len() - 1;
        self.update_chain_indexes_in_board(chain_idx);
        chain_idx
    }

    fn add_stone_to_chain(&mut self, chain_idx: ChainIdx, stone: BoardIdx) {
        let chain = &mut self.chains[chain_idx];
        if stone < chain.origin as usize {
            // replace origin
            self.next_stone[stone] = chain.origin;
            self.next_stone[chain.last as usize] = stone as u16;
            chain.origin = stone as u16;
        } else {
            self.next_stone[chain.last as usize] = stone as u16;
            self.next_stone[stone] = chain.origin;
            chain.last = stone as u16;
        }
        chain.num_stones += 1;
        debug_assert_eq!(
            iter_stones!(self, chain_idx).last().unwrap() as u16,
            self.chains[chain_idx].last
        );
    }

    fn merge_strings(&mut self, chain1_idx: ChainIdx, chain2_idx: ChainIdx) {
        debug_assert_eq!(
            self.chains[chain1_idx].color, self.chains[chain2_idx].color,
            "Cannot merge two strings of different color"
        );
        debug_assert_ne!(chain1_idx, chain2_idx, "merging the same string");

        let (chain1, chain2) = if chain1_idx < chain2_idx {
            let (s1, s2) = self.chains.split_at_mut(chain2_idx);
            (&mut s1[chain1_idx], s2.first_mut().unwrap())
        } else {
            // ren2_idx > ren1_idx
            let (contains_chain2, contains_ren1) = self.chains.split_at_mut(chain1_idx);
            (
                contains_ren1.first_mut().unwrap(),
                &mut contains_chain2[chain2_idx],
            )
        };
        merge(&mut chain1.liberties, &chain2.liberties);

        let chain1_last = chain1.last;
        let chain2_last = chain2.last;

        let chain1_origin = chain1.origin;
        let chain2_origin = chain2.origin;

        if chain1_origin > chain2_origin {
            chain1.origin = chain2_origin;
        } else {
            chain1.last = chain2_last;
        }
        self.next_stone
            .swap(chain1_last as usize, chain2_last as usize);
        chain1.num_stones += chain2.num_stones;

        self.update_chain_indexes_in_board(chain1_idx);
        self.put_chain_in_bin(chain2_idx);
    }

    #[inline]
    fn put_chain_in_bin(&mut self, ren_idx: ChainIdx) {
        self.chains[ren_idx].used = false;
        //self.free_slots.set(ren_idx, true);
    }

    #[allow(dead_code)]
    #[cfg(debug_assertions)]
    fn check_integrity_ren(&self, ren_idx: ChainIdx) {
        assert_eq!(
            iter_stones!(self, ren_idx).next().unwrap() as u16,
            self.chains[ren_idx].origin,
            "The origin doesn't match"
        );
        assert_eq!(
            iter_stones!(self, ren_idx).last().unwrap() as u16,
            self.chains[ren_idx].last,
            "The last doesn't match"
        );
        if iter_stones!(self, ren_idx).count() as u16 != self.chains[ren_idx].num_stones {
            panic!("The number of stones don't match")
        }
    }

    #[allow(dead_code)]
    #[cfg(debug_assertions)]
    fn check_integrity_all(&self) {
        for ren_idx in (0..self.chains.len()).filter(|&ren_idx| self.chains[ren_idx].used) {
            self.check_integrity_ren(ren_idx);
        }
    }
}

impl Display for Goban {
    fn fmt(&self, f: &mut Formatter) -> Result<(), Error> {
        write!(f, "{}", self.pretty_string())
    }
}

impl PartialEq for Goban {
    fn eq(&self, other: &Goban) -> bool {
        other.zobrist_hash == self.zobrist_hash
    }
}

impl Hash for Goban {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.zobrist_hash.hash(state)
    }
}

impl Eq for Goban {}

impl Default for Goban {
    fn default() -> Self {
        Goban::new((19, 19))
    }
}
