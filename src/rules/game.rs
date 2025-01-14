use hash_hasher::{HashBuildHasher, HashedSet};

use crate::pieces::goban::*;
use crate::pieces::stones::Color::{Black, White};
use crate::pieces::stones::{Color, Stone, EMPTY};
use crate::pieces::util::coord::{corner_points, is_coord_valid, two_to_1dim, Coord, Size};
use crate::pieces::Nat;
use crate::rules::EndGame::{Draw, WinnerByScore};
use crate::rules::Rule;
use crate::rules::{EndGame, GobanSizes, IllegalRules, Move, ScoreRules};
use crate::rules::{PlayError, CHINESE};

/// Most important struct of the library, it's the entry point.
/// It represents a Game of Go.
#[derive(Clone, Getters, CopyGetters, Setters, Debug)]
pub struct Game {
    #[get = "pub"]
    pub(super) goban: Goban,

    #[get_copy = "pub"]
    pub(super) passes: u32,

    #[get_copy = "pub"]
    pub(super) prisoners: (u32, u32),

    /// None if the game is not finished,
    pub(super) outcome: Option<EndGame>,

    #[get_copy = "pub"]
    pub(super) turn: Color,

    #[get_copy = "pub"]
    #[set = "pub"]
    pub(super) rule: Rule,

    #[get_copy = "pub"]
    pub(super) handicap: u32,

    #[cfg(feature = "history")]
    #[get = "pub"]
    pub(super) history: Vec<Goban>,

    #[get = "pub"]
    pub(super) last_hash: u64,

    pub(super) hashes: HashedSet<u64>,

    pub(super) ko_point: Option<Coord>,
}

impl Game {
    /// Crates a new game for playing Go
    pub fn new(size: GobanSizes, rule: Rule) -> Self {
        let (h, w) = size.into();
        let goban = Goban::new(size.into());
        let length = h as usize * w as usize;
        #[cfg(feature = "history")]
        let history = Vec::with_capacity(length);
        let prisoners = (0, 0);
        let handicap = 0;
        let hashes = HashedSet::with_capacity_and_hasher(length, HashBuildHasher::default());
        Self {
            goban,
            turn: Color::Black,
            prisoners,
            passes: 0,
            #[cfg(feature = "history")]
            history,
            outcome: None,
            rule,
            handicap,
            hashes,
            last_hash: 0,
            ko_point: None,
        }
    }
}

impl Game {
    /// Resume the game when to players have passed, and want to continue.
    #[inline]
    pub fn resume(&mut self) {
        self.passes = 0;
    }

    #[inline]
    pub fn set_komi(&mut self, komi: f32) {
        self.rule.komi = komi;
    }

    #[inline]
    pub fn komi(&self) -> f32 {
        self.rule.komi
    }

    #[inline]
    pub fn size(&self) -> Size {
        self.goban.size()
    }

    /// True when the game is over (two passes, or no more legals moves, Resign)
    #[inline]
    pub fn is_over(&self) -> bool {
        if self.outcome.is_some() {
            true
        } else {
            self.passes >= 2
        }
    }

    /// Returns the endgame.
    /// None if the game is not finished
    #[inline]
    pub fn outcome(&self) -> Option<EndGame> {
        if !self.is_over() {
            None
        } else if self.outcome.is_some() {
            self.outcome
        } else {
            // two passes
            let scores = self.calculate_score();
            if (scores.0 - scores.1).abs() < f32::EPSILON {
                Some(Draw)
            } else if scores.0 > scores.1 {
                Some(WinnerByScore(Black, scores.0 - scores.1))
            } else {
                Some(WinnerByScore(White, scores.1 - scores.0))
            }
        }
    }

    /// Generate all moves on all empty intersections. Lazy.
    #[inline]
    pub fn pseudo_legals(&self) -> impl Iterator<Item = Coord> + '_ {
        self.goban.get_empty_coords()
    }

    /// Get all moves on all empty intersections.
    pub fn pseudo_legals_vec(&self) -> Vec<Coord> {
        let size = self.size();
        let mut vec = Vec::with_capacity((size.0 * size.1) as usize);
        let board = self.goban.board();
        for i in 0..size.0 as Nat {
            for j in 0..size.1 as Nat {
                if board[two_to_1dim(size, (i, j))].is_none() {
                    vec.push((i, j));
                }
            }
        }
        vec
    }

    /// Returns a list with legals moves. from the rule specified in at the creation.
    #[inline]
    pub fn legals(&self) -> impl Iterator<Item = Coord> + '_ {
        self.legals_by(self.rule.flag_illegal)
    }

    /// Return a list with the legals moves. doesn't take the rule specified in the game but take
    /// the one passed on parameter.
    #[inline]
    pub fn legals_by(&self, legals_rules: IllegalRules) -> impl Iterator<Item = Coord> + '_ {
        self.pseudo_legals()
            .filter(move |&s| self.check_point_by(s, legals_rules).is_none())
    }

    /// Method to play on the goban or pass.
    /// (0,0) is in the top left corner of the goban.
    ///
    /// # Panics
    ///
    /// If the coordinates of the move are outside the board.
    pub fn play(&mut self, play: Move) -> &mut Self {
        match play {
            Move::Pass => {
                assert!(self.passes < 2, "This game is already paused");
                self.turn = !self.turn;
                self.passes += 1;
                self
            }
            Move::Play(x, y) => {
                let hash = self.goban.zobrist_hash();
                self.last_hash = hash;
                self.hashes.insert(hash);
                #[cfg(feature = "history")]
                self.history.push(self.goban.clone());
                let (dead_rens, added_ren) = self.goban.push_wth_feedback((x, y), self.turn);
                self.ko_point = None;
                self.remove_captured_stones(&dead_rens, added_ren);
                //self.prisoners = self.remove_captured_stones();
                self.turn = !self.turn;
                self.passes = 0;
                self
            }
            Move::Resign(player) => {
                self.outcome = Some(EndGame::WinnerByResign(player));
                self
            }
        }
    }

    /// This methods plays a move then return the hash of the goban simulated,
    /// used in legals for fast move simulation in Super Ko situations.
    pub fn play_for_verification(&self, (x, y): Coord) -> u64 {
        let mut test_goban = self.goban.clone();
        let (dead_go_strings, added_ren) = test_goban.push_wth_feedback((x, y), self.turn);
        test_goban.remove_captured_stones_aux(
            self.turn,
            !self.rule.flag_illegal.contains(IllegalRules::SUICIDE),
            self.prisoners,
            &dead_go_strings,
            added_ren,
        );
        test_goban.zobrist_hash()
    }

    pub fn try_play_color(&mut self, color: Color, play: Move) -> Result<&mut Self, PlayError> {
        self.turn = color;
        // TODO: we don't undo color if we fail.
        return self.try_play(play);
    }

    /// Method to play but it verifies if the play is legal or not.
    ///
    /// # Errors
    ///
    /// If the move is a suicide Move return SuicideMove
    /// If the move is a Ko Move returns Ko
    /// if point is already filled then return PointNotEmpty
    /// If the game is paused then return GamePaused
    pub fn try_play(&mut self, play: Move) -> Result<&mut Self, PlayError> {
        if self.passes == 2 {
            Err(PlayError::GamePaused)
        } else {
            match play {
                Move::Play(x, y) => {
                    if self.goban.get_color((x, y)) != EMPTY {
                        Err(PlayError::PointNotEmpty)
                    } else if let Some(c) = self.check_point((x as Nat, y as Nat)) {
                        Err(c)
                    } else {
                        Ok(self.play(play))
                    }
                }
                Move::Pass | Move::Resign(_) => Ok(self.play(play)),
            }
        }
    }

    /// Put the handicap stones on the goban.
    /// This put the turn for white but doesn't update the komi.
    pub fn put_handicap(&mut self, points: &[Coord]) {
        self.handicap = points.len() as u32;
        points.iter().for_each(|&coord| {
            self.goban.push(coord, Color::Black);
        });
        self.turn = Color::White;
    }

    /// Calculates score. with prisoners and komi.
    /// Dependant of the rule in the game.
    #[inline]
    pub fn calculate_score(&self) -> (f32, f32) {
        self.calculate_score_by(self.rule.flag_score)
    }

    /// Calculates the score by the rule passed in parameter.
    pub fn calculate_score_by(&self, rule: ScoreRules) -> (f32, f32) {
        let (black_score, white_score) = self.goban.calculate_territories();
        let mut black_score = black_score as f32;
        let mut white_score = white_score as f32;
        if rule.contains(ScoreRules::PRISONNERS) {
            black_score += self.prisoners.0 as f32;
            white_score += self.prisoners.1 as f32;
        }
        if rule.contains(ScoreRules::STONES) {
            let (black_stones, white_stones) = self.goban.number_of_stones();
            black_score += black_stones as f32;
            white_score += white_stones as f32;
        }
        if rule.contains(ScoreRules::KOMI) {
            white_score += self.komi();
        }

        (black_score, white_score)
    }

    /// Returns true if the stone played in that point will capture another
    /// string.
    pub fn will_capture(&self, point: Coord) -> bool {
        self.goban
            .get_neighbors_chains(point)
            .filter(|go_str| go_str.color != self.turn)
            // if an enemy string has only liberty it's a capture move
            .any(|go_str| go_str.is_atari())
    }

    /// Test if a point is legal or not for the current player,
    #[inline]
    pub fn check_point(&self, point: Coord) -> Option<PlayError> {
        self.check_point_by(point, self.rule.flag_illegal)
    }

    /// Test if a point is legal or not by the rule passed in parameter.
    pub fn check_point_by(&self, point: Coord, illegal_rules: IllegalRules) -> Option<PlayError> {
        let stone = Stone {
            coord: point,
            color: self.turn,
        };
        if !self.goban.get_point(point).is_empty() {
            Some(PlayError::PointNotEmpty)
        } else if illegal_rules.contains(IllegalRules::KO) && self.check_ko(stone) {
            Some(PlayError::Ko)
        } else if illegal_rules.contains(IllegalRules::SUICIDE) && self.check_suicide(stone) {
            Some(PlayError::Suicide)
        } else if illegal_rules.contains(IllegalRules::FILLEYE) && self.check_eye(stone) {
            Some(PlayError::FillEye)
        } else if illegal_rules.contains(IllegalRules::SUPERKO) && self.check_superko(stone) {
            Some(PlayError::Ko)
        } else {
            None
        }
    }

    // Return the number of allied corner and off board corners.
    fn helper_check_eye(&self, point: (Nat, Nat), color: Color) -> (Nat, Nat) {
        let mut corner_ally = 0;
        let mut corner_off_board = 0;
        for p in corner_points(point) {
            if is_coord_valid(self.goban.size(), p) {
                if self.goban.get_color(p) == Some(color) {
                    corner_ally += 1
                }
            } else {
                corner_off_board += 1;
            }
        }

        (corner_ally, corner_off_board)
    }

    /// Detects true eyes. return true is the stone is an eye.
    /// Except for this form :
    /// ```{nothing}
    ///  ++
    ///  + ++
    ///  ++ +
    ///    ++
    /// ```
    /// This function is only used for performance checking in the rules,
    /// and not for checking is a point is really an eye !
    pub fn check_eye(&self, Stone { coord, color }: Stone) -> bool {
        // if the intersection is not empty
        if self.goban.get_color(coord).is_some() {
            return false;
        }

        // if he doesn't have the cross then return false
        if self
            .goban
            .get_neighbors_points(coord)
            .any(|s| s.color != Some(color))
        {
            return false;
        }

        let (corner_ally, corner_off_board) = self.helper_check_eye(coord, color);
        let corners = corner_ally + corner_off_board;
        if corners == 4 {
            return true;
        }

        if corners == 3 || corners == 2 {
            for s in corner_points(coord)
                .into_iter()
                .filter(move |p| is_coord_valid(self.goban.size(), *p))
                .filter_map(move |c| Some(self.goban.get_point(c)).filter(|p| p.is_empty()))
            {
                if self
                    .goban
                    .get_neighbors_stones(s.coord)
                    .any(move |s| s.color != color)
                {
                    return false;
                }
                let (ca, cof) = self.helper_check_eye(s.coord, color);
                let c = ca + cof;
                if c == 3 || c == 2 {
                    return true;
                }
            }
        }
        false
    }

    /// Test if a play is ko.
    /// If the goban is in the configuration of two plays ago returns true
    pub fn check_ko(&self, stone: Stone) -> bool {
        self.ko_point == Some(stone.coord)
    }

    /// Rule of the super Ko, if any before configuration was already played then return true.
    pub fn check_superko(&self, stone: Stone) -> bool {
        if self.last_hash == 0 || self.hashes.len() <= 2 || !self.will_capture(stone.coord) {
            false
        } else {
            self.check_ko(stone)
                || self
                    .hashes
                    .contains(&self.play_for_verification(stone.coord))
        }
    }

    /// Add a stone to the board an then test if the stone or stone group is
    /// dead.
    /// Returns true if the move is a suicide
    pub fn check_suicide(&self, stone: Stone) -> bool {
        if self.goban.has_liberties(stone.coord) {
            false
        } else {
            !self
                .goban
                .get_neighbors_chains(stone.coord)
                .any(|neighbor_go_string| {
                    if neighbor_go_string.color == stone.color {
                        // Connecting with an other string which is not in danger
                        !neighbor_go_string.is_atari()
                    } else {
                        // Capture move
                        neighbor_go_string.is_atari()
                    }
                })
        }
    }

    /// Displays the internal board.
    pub fn display_goban(&self) {
        println!("{}", self.goban)
    }

    #[inline]
    fn remove_captured_stones(&mut self, dead_chains: &[ChainIdx], added_chain: ChainIdx) {
        let res = self.goban.remove_captured_stones_aux(
            self.turn,
            !self.rule.flag_illegal.contains(IllegalRules::SUICIDE),
            self.prisoners,
            dead_chains,
            added_chain,
        );
        self.prisoners = res.0;
        self.ko_point = res.1;
    }
}

impl Default for Game {
    fn default() -> Self {
        Game::new(GobanSizes::Nineteen, CHINESE)
    }
}
