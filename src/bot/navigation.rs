use std::collections::VecDeque;

use bevy::prelude::*;

use crate::arena::{
    GroundSupport, SplitCausewayDoorState, ground_support_for_arena_with_radius,
    navigation_segment_clear_for_arena, navigation_segment_clear_with_doors_for_arena,
};
use crate::arena_defs::{ArenaDefinition, arena_definition, arena_definitions};
use crate::constants::{
    DASH_JUMP_MIN_FORWARD_SPEED, FIGHTER_COUNT, FIGHTER_RADIUS, GRAVITY, JUMP_SPEED,
    MAX_AIR_SPEED,
};

const NAV_CELL_SIZE: f32 = 0.75;
const NAV_MAX_GRID_SIDE: usize = 64;
const NAV_MAX_NODES: usize = NAV_MAX_GRID_SIDE * NAV_MAX_GRID_SIDE;
const NAV_MAX_EDGES: usize = 56;
const NAV_AIR_TIME: f32 = JUMP_SPEED * 2.0 / GRAVITY;
const NAV_MAX_JUMP_DISTANCE: f32 = MAX_AIR_SPEED * NAV_AIR_TIME;
const NAV_MAX_DASH_JUMP_DISTANCE: f32 = DASH_JUMP_MIN_FORWARD_SPEED * NAV_AIR_TIME;
const NAV_MAX_NEAREST_DISTANCE: f32 = 2.25;
const NAV_MAX_STEP_HEIGHT: f32 = 0.28;
const NAV_HEIGHT_EPSILON: f32 = NAV_MAX_STEP_HEIGHT;
const NAV_MAX_JUMP_RISE: f32 = JUMP_SPEED * JUMP_SPEED / (2.0 * GRAVITY);
const NAV_MAX_DROP_HEIGHT: f32 = 2.40;
const NAV_JUMP_CLEARANCE: f32 = 0.90;
const NAV_WAYPOINT_REACHED_DISTANCE: f32 = 0.42;
const NAV_DESTINATION_REPLAN_DISTANCE: f32 = NAV_CELL_SIZE;
const NAV_PROGRESS_EPSILON: f32 = 0.05;
const NAV_STALL_DECISION_TICKS: u64 = 12;
const NAV_MAX_PATH_NODES: usize = 128;
const INVALID_NODE: NavigationNodeId = NavigationNodeId::MAX;

type NavigationNodeId = u16;

pub(crate) const MAX_NAVIGATION_BLOCKERS: usize = 16;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct NavigationBlocker {
    pub center: Vec2,
    pub radius: f32,
}

#[derive(Clone, Debug)]
pub(crate) struct NavigationBlockers {
    blockers: [NavigationBlocker; MAX_NAVIGATION_BLOCKERS],
    len: u8,
}

impl Default for NavigationBlockers {
    fn default() -> Self {
        Self {
            blockers: [NavigationBlocker::default(); MAX_NAVIGATION_BLOCKERS],
            len: 0,
        }
    }
}

impl NavigationBlockers {
    pub(crate) fn clear(&mut self) {
        self.len = 0;
    }

    pub(crate) fn push(&mut self, center: Vec2, radius: f32) -> bool {
        if self.len as usize >= MAX_NAVIGATION_BLOCKERS || !radius.is_finite() || radius <= 0.0 {
            return false;
        }
        self.blockers[self.len as usize] = NavigationBlocker { center, radius };
        self.len += 1;
        true
    }

    fn iter(&self) -> impl Iterator<Item = &NavigationBlocker> {
        self.blockers[..self.len as usize].iter()
    }

    fn fingerprint(&self) -> u64 {
        let mut fingerprint = 0xcbf2_9ce4_8422_2325_u64;
        for blocker in self.iter() {
            for bits in [
                blocker.center.x.to_bits(),
                blocker.center.y.to_bits(),
                blocker.radius.to_bits(),
            ] {
                fingerprint ^= bits as u64;
                fingerprint = fingerprint.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        fingerprint ^ self.len as u64
    }

    fn blocks_segment(&self, from: Vec3, to: Vec3, fighter_radius: f32) -> bool {
        let start = Vec2::new(from.x, from.z);
        let end = Vec2::new(to.x, to.z);
        let segment = end - start;
        let segment_length_squared = segment.length_squared();
        self.iter().any(|blocker| {
            let expanded_radius = blocker.radius + fighter_radius;
            let start_distance_squared = start.distance_squared(blocker.center);
            let end_distance_squared = end.distance_squared(blocker.center);
            if start_distance_squared <= expanded_radius * expanded_radius
                && end_distance_squared > start_distance_squared + 0.000_1
            {
                return false;
            }
            let amount = if segment_length_squared > 0.000_001 {
                ((blocker.center - start).dot(segment) / segment_length_squared).clamp(0.0, 1.0)
            } else {
                0.0
            };
            (start + segment * amount).distance_squared(blocker.center)
                <= expanded_radius * expanded_radius
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NavigationTraversal {
    Walk,
    Jump,
    DashJump,
    Drop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NavigationEdge {
    to: NavigationNodeId,
    traversal: NavigationTraversal,
}

#[derive(Clone, Copy, Debug)]
struct NavigationNode {
    position: Vec3,
    grid_x: u16,
    grid_z: u16,
    edges: [Option<NavigationEdge>; NAV_MAX_EDGES],
}

#[derive(Debug)]
struct ArenaNavigationGraph {
    width: usize,
    depth: usize,
    cells: Vec<NavigationNodeId>,
    nodes: Vec<NavigationNode>,
}

impl ArenaNavigationGraph {
    fn build(arena: &ArenaDefinition) -> Result<Self, &'static str> {
        let side = ((arena.ringout_radius * 2.0 / NAV_CELL_SIZE).ceil() as usize) + 1;
        if side > NAV_MAX_GRID_SIDE {
            return Err("arena navigation grid exceeds NAV_MAX_GRID_SIDE");
        }

        let width = side;
        let depth = side;
        let origin = -((side - 1) as f32 * NAV_CELL_SIZE) * 0.5;
        let mut cells = vec![INVALID_NODE; width * depth];
        let mut nodes = Vec::with_capacity(width * depth);

        for grid_z in 0..depth {
            for grid_x in 0..width {
                let x = origin + grid_x as f32 * NAV_CELL_SIZE;
                let z = origin + grid_z as f32 * NAV_CELL_SIZE;
                if Vec2::new(x, z).length() > arena.ringout_radius {
                    continue;
                }
                let GroundSupport::Firm(height) =
                    ground_support_for_arena_with_radius(arena, x, z, 0.0)
                else {
                    continue;
                };
                if !fighter_footprint_is_supported(arena, Vec2::new(x, z), height) {
                    continue;
                }
                let occupancy_probe = Vec3::new(x, height + 0.04, z);
                if !navigation_segment_clear_for_arena(
                    arena,
                    occupancy_probe,
                    occupancy_probe,
                    FIGHTER_RADIUS,
                ) {
                    continue;
                }
                if nodes.len() >= NAV_MAX_NODES {
                    return Err("arena navigation graph exceeds NAV_MAX_NODES");
                }

                let node_id = nodes.len() as NavigationNodeId;
                cells[grid_z * width + grid_x] = node_id;
                nodes.push(NavigationNode {
                    position: Vec3::new(x, height, z),
                    grid_x: grid_x as u16,
                    grid_z: grid_z as u16,
                    edges: [None; NAV_MAX_EDGES],
                });
            }
        }

        let mut graph = Self {
            width,
            depth,
            cells,
            nodes,
        };
        graph.build_edges(arena);
        Ok(graph)
    }

    fn build_edges(&mut self, arena: &ArenaDefinition) {
        const ADJACENT_DIRECTIONS: [(isize, isize); 8] = [
            (1, 0),
            (0, 1),
            (-1, 0),
            (0, -1),
            (1, 1),
            (-1, 1),
            (-1, -1),
            (1, -1),
        ];
        const DASH_JUMP_DIRECTIONS: [(isize, isize); 8] = ADJACENT_DIRECTIONS;

        for node_index in 0..self.nodes.len() {
            let from = self.nodes[node_index];
            let grid_x = from.grid_x as isize;
            let grid_z = from.grid_z as isize;
            let mut edges = [None; NAV_MAX_EDGES];

            for (dx, dz) in ADJACENT_DIRECTIONS {
                if dx != 0
                    && dz != 0
                    && (self.node_at(grid_x + dx, grid_z).is_none()
                        || self.node_at(grid_x, grid_z + dz).is_none())
                {
                    continue;
                }
                let Some(to) = self.node_at(grid_x + dx, grid_z + dz) else {
                    continue;
                };
                if let Some(edge) = self.edge_between(arena, from.position, to, false) {
                    insert_edge(&mut edges, edge);
                }
            }

            let max_jump_cells = (NAV_MAX_JUMP_DISTANCE / NAV_CELL_SIZE).floor() as isize;
            let max_jump_cell_distance_squared = max_jump_cells * max_jump_cells;
            for cell_distance_squared in 4..=max_jump_cell_distance_squared {
                for dz in -max_jump_cells..=max_jump_cells {
                    for dx in -max_jump_cells..=max_jump_cells {
                        if dx * dx + dz * dz != cell_distance_squared {
                            continue;
                        }
                        let Some(to) = self.node_at(grid_x + dx, grid_z + dz) else {
                            continue;
                        };
                        let destination = self.nodes[to as usize].position;
                        if !jump_segment_crosses_gap(arena, from.position, destination)
                            && !self.grid_segment_has_missing_node(grid_x, grid_z, dx, dz)
                        {
                            continue;
                        }
                        if let Some(edge) = self.edge_between(arena, from.position, to, true) {
                            insert_edge(&mut edges, edge);
                        }
                    }
                }
            }

            let max_dash_jump_cells =
                (NAV_MAX_DASH_JUMP_DISTANCE / NAV_CELL_SIZE).floor() as isize;
            for (direction_x, direction_z) in DASH_JUMP_DIRECTIONS {
                for distance in 2..=max_dash_jump_cells {
                    let dx = direction_x * distance;
                    let dz = direction_z * distance;
                    let horizontal_distance =
                        Vec2::new(dx as f32, dz as f32).length() * NAV_CELL_SIZE;
                    if horizontal_distance <= NAV_MAX_JUMP_DISTANCE
                        || horizontal_distance > NAV_MAX_DASH_JUMP_DISTANCE
                    {
                        continue;
                    }
                    let Some(to) = self.node_at(grid_x + dx, grid_z + dz) else {
                        continue;
                    };
                    let destination = self.nodes[to as usize].position;
                    if !jump_segment_crosses_gap(arena, from.position, destination)
                        && !self.grid_segment_has_missing_node(grid_x, grid_z, dx, dz)
                    {
                        continue;
                    }
                    if let Some(mut edge) = self.edge_between(arena, from.position, to, true) {
                        edge.traversal = NavigationTraversal::DashJump;
                        insert_edge(&mut edges, edge);
                        break;
                    }
                }
            }

            self.nodes[node_index].edges = edges;
        }
    }

    fn edge_between(
        &self,
        arena: &ArenaDefinition,
        from: Vec3,
        to: NavigationNodeId,
        crosses_gap: bool,
    ) -> Option<NavigationEdge> {
        let destination = self.nodes[to as usize].position;
        let height_delta = destination.y - from.y;
        let traversal = if crosses_gap {
            if walk_surface_is_continuous(arena, from, destination) {
                NavigationTraversal::Walk
            } else if height_delta > NAV_MAX_JUMP_RISE || height_delta < -NAV_MAX_DROP_HEIGHT {
                return None;
            } else if height_delta < -NAV_MAX_STEP_HEIGHT {
                NavigationTraversal::Drop
            } else {
                NavigationTraversal::Jump
            }
        } else if height_delta.abs() <= NAV_MAX_STEP_HEIGHT {
            if !walk_surface_is_continuous(arena, from, destination) {
                return None;
            }
            NavigationTraversal::Walk
        } else if height_delta > 0.0 && height_delta <= NAV_MAX_JUMP_RISE {
            NavigationTraversal::Jump
        } else if height_delta < 0.0 && height_delta >= -NAV_MAX_DROP_HEIGHT {
            NavigationTraversal::Drop
        } else {
            return None;
        };

        let clearance = match traversal {
            NavigationTraversal::Walk => 0.04,
            NavigationTraversal::Jump
            | NavigationTraversal::DashJump
            | NavigationTraversal::Drop => NAV_JUMP_CLEARANCE,
        };
        let sweep_height = from.y.max(destination.y) + clearance;
        let sweep_from = Vec3::new(from.x, sweep_height, from.z);
        let sweep_to = Vec3::new(destination.x, sweep_height, destination.z);
        navigation_segment_clear_for_arena(arena, sweep_from, sweep_to, FIGHTER_RADIUS).then_some(
            NavigationEdge {
                to,
                traversal,
            },
        )
    }

    fn node_at(&self, x: isize, z: isize) -> Option<NavigationNodeId> {
        if x < 0 || z < 0 || x >= self.width as isize || z >= self.depth as isize {
            return None;
        }
        let node = self.cells[z as usize * self.width + x as usize];
        (node != INVALID_NODE).then_some(node)
    }

    fn grid_segment_has_missing_node(
        &self,
        from_x: isize,
        from_z: isize,
        dx: isize,
        dz: isize,
    ) -> bool {
        let steps = dx.abs().max(dz.abs());
        (1..steps).any(|step| {
            let amount = step as f32 / steps as f32;
            let x = from_x + (dx as f32 * amount).round() as isize;
            let z = from_z + (dz as f32 * amount).round() as isize;
            self.node_at(x, z).is_none()
        })
    }

    fn nearest_node(&self, position: Vec3) -> Option<NavigationNodeId> {
        let max_distance_squared = NAV_MAX_NEAREST_DISTANCE * NAV_MAX_NEAREST_DISTANCE;
        let mut best = None;
        let mut best_distance_squared = max_distance_squared;
        for (index, node) in self.nodes.iter().enumerate() {
            let distance_squared = node.position.distance_squared(position);
            if distance_squared < best_distance_squared {
                best = Some(index as NavigationNodeId);
                best_distance_squared = distance_squared;
            }
        }
        best
    }
}

fn fighter_footprint_is_supported(arena: &ArenaDefinition, center: Vec2, height: f32) -> bool {
    const DIRECTIONS: [Vec2; 8] = [
        Vec2::X,
        Vec2::Y,
        Vec2::NEG_X,
        Vec2::NEG_Y,
        Vec2::new(0.707_106_77, 0.707_106_77),
        Vec2::new(-0.707_106_77, 0.707_106_77),
        Vec2::new(-0.707_106_77, -0.707_106_77),
        Vec2::new(0.707_106_77, -0.707_106_77),
    ];
    DIRECTIONS.into_iter().all(|direction| {
        let probe = center + direction * (FIGHTER_RADIUS * 0.9);
        ground_support_for_arena_with_radius(arena, probe.x, probe.y, 0.0)
            .height()
            .is_some_and(|probe_height| (probe_height - height).abs() <= NAV_HEIGHT_EPSILON)
    })
}

fn walk_surface_is_continuous(arena: &ArenaDefinition, from: Vec3, to: Vec3) -> bool {
    [0.25, 0.5, 0.75].into_iter().all(|amount| {
        let probe = from.lerp(to, amount);
        ground_support_for_arena_with_radius(arena, probe.x, probe.z, 0.0)
            .height()
            .is_some_and(|height| (height - probe.y).abs() <= NAV_MAX_STEP_HEIGHT)
    })
}

fn jump_segment_crosses_gap(arena: &ArenaDefinition, from: Vec3, to: Vec3) -> bool {
    let horizontal_distance = Vec2::new(to.x - from.x, to.z - from.z).length();
    let probe_count = (horizontal_distance / (NAV_CELL_SIZE * 0.5)).ceil() as usize;
    if probe_count < 2 {
        return false;
    }
    (1..probe_count).any(|probe| {
        let amount = probe as f32 / probe_count as f32;
        let position = from.lerp(to, amount);
        ground_support_for_arena_with_radius(arena, position.x, position.z, 0.0)
            .height()
            .is_none_or(|height| (height - position.y).abs() > NAV_MAX_STEP_HEIGHT)
    })
}

fn insert_edge(edges: &mut [Option<NavigationEdge>; NAV_MAX_EDGES], edge: NavigationEdge) {
    if edges.iter().flatten().any(|existing| existing.to == edge.to) {
        return;
    }
    if let Some(slot) = edges.iter_mut().find(|slot| slot.is_none()) {
        *slot = Some(edge);
    }
}

#[derive(Clone, Debug)]
struct CachedNavigationRoute {
    valid: bool,
    arena_index: usize,
    goal: NavigationNodeId,
    destination: Vec3,
    blocker_fingerprint: u64,
    nodes: [NavigationNodeId; NAV_MAX_PATH_NODES],
    traversals: [NavigationTraversal; NAV_MAX_PATH_NODES],
    len: usize,
    cursor: usize,
    best_waypoint_distance: f32,
    last_progress_tick: u64,
    revision: u64,
}

impl Default for CachedNavigationRoute {
    fn default() -> Self {
        Self {
            valid: false,
            arena_index: usize::MAX,
            goal: INVALID_NODE,
            destination: Vec3::ZERO,
            blocker_fingerprint: 0,
            nodes: [INVALID_NODE; NAV_MAX_PATH_NODES],
            traversals: [NavigationTraversal::Walk; NAV_MAX_PATH_NODES],
            len: 0,
            cursor: 0,
            best_waypoint_distance: f32::INFINITY,
            last_progress_tick: 0,
            revision: 0,
        }
    }
}

impl CachedNavigationRoute {
    fn invalidate(&mut self) {
        self.valid = false;
    }

    fn begin_plan(
        &mut self,
        arena_index: usize,
        goal: NavigationNodeId,
        destination: Vec3,
        blocker_fingerprint: u64,
        decision_tick: u64,
    ) {
        self.valid = true;
        self.arena_index = arena_index;
        self.goal = goal;
        self.destination = destination;
        self.blocker_fingerprint = blocker_fingerprint;
        self.len = 0;
        self.cursor = 0;
        self.best_waypoint_distance = f32::INFINITY;
        self.last_progress_tick = decision_tick;
        self.revision = self.revision.wrapping_add(1);
    }

    fn matches(
        &self,
        arena_index: usize,
        goal: NavigationNodeId,
        destination: Vec3,
        blocker_fingerprint: u64,
    ) -> bool {
        self.valid
            && self.arena_index == arena_index
            && self.goal == goal
            && self.blocker_fingerprint == blocker_fingerprint
            && Vec2::new(
                destination.x - self.destination.x,
                destination.z - self.destination.z,
            )
            .length_squared()
                <= NAV_DESTINATION_REPLAN_DISTANCE * NAV_DESTINATION_REPLAN_DISTANCE
    }

    fn advance_reached_waypoints(&mut self, graph: &ArenaNavigationGraph, from: Vec3, tick: u64) {
        while self.cursor < self.len {
            let waypoint = graph.nodes[self.nodes[self.cursor] as usize].position;
            let distance = Vec2::new(waypoint.x - from.x, waypoint.z - from.z).length();
            if distance > NAV_WAYPOINT_REACHED_DISTANCE {
                break;
            }
            self.cursor += 1;
            self.best_waypoint_distance = f32::INFINITY;
            self.last_progress_tick = tick;
        }
    }

    fn record_progress(&mut self, distance: f32, tick: u64) -> bool {
        if distance + NAV_PROGRESS_EPSILON < self.best_waypoint_distance {
            self.best_waypoint_distance = distance;
            self.last_progress_tick = tick;
            return true;
        }
        tick.saturating_sub(self.last_progress_tick) < NAV_STALL_DECISION_TICKS
    }
}

#[derive(Debug)]
struct NavigationSearchScratch {
    generation: u32,
    visited_generation: Vec<u32>,
    parent: Vec<NavigationNodeId>,
    parent_traversal: Vec<NavigationTraversal>,
    queue: VecDeque<NavigationNodeId>,
}

impl Default for NavigationSearchScratch {
    fn default() -> Self {
        Self {
            generation: 0,
            visited_generation: vec![0; NAV_MAX_NODES],
            parent: vec![INVALID_NODE; NAV_MAX_NODES],
            parent_traversal: vec![NavigationTraversal::Walk; NAV_MAX_NODES],
            queue: VecDeque::with_capacity(NAV_MAX_NODES),
        }
    }
}

impl NavigationSearchScratch {
    fn begin(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.visited_generation.fill(0);
            self.generation = 1;
        }
        self.queue.clear();
    }

    fn build_route(
        &mut self,
        graph: &ArenaNavigationGraph,
        start: NavigationNodeId,
        goal: NavigationNodeId,
        arena: &ArenaDefinition,
        blockers: &NavigationBlockers,
        doors: &SplitCausewayDoorState,
        route: &mut CachedNavigationRoute,
    ) -> bool {
        if start == goal {
            route.len = 0;
            route.cursor = 0;
            return true;
        }

        self.begin();
        self.visited_generation[start as usize] = self.generation;
        self.parent[start as usize] = INVALID_NODE;
        self.queue.push_back(start);

        let mut found = false;
        while let Some(current) = self.queue.pop_front() {
            for edge in graph.nodes[current as usize].edges.iter().flatten() {
                let next = edge.to as usize;
                if self.visited_generation[next] == self.generation {
                    continue;
                }
                if !navigation_edge_is_clear(graph, current, *edge, arena, blockers, doors) {
                    continue;
                }
                self.visited_generation[next] = self.generation;
                self.parent[next] = current;
                self.parent_traversal[next] = edge.traversal;
                if edge.to == goal {
                    found = true;
                    break;
                }
                self.queue.push_back(edge.to);
            }
            if found {
                break;
            }
        }
        found && self.reconstruct_route(start, goal, route)
    }

    fn reconstruct_route(
        &self,
        start: NavigationNodeId,
        goal: NavigationNodeId,
        route: &mut CachedNavigationRoute,
    ) -> bool {
        let mut reverse_nodes = [INVALID_NODE; NAV_MAX_PATH_NODES];
        let mut reverse_traversals = [NavigationTraversal::Walk; NAV_MAX_PATH_NODES];
        let mut current = goal;
        let mut len = 0;
        while current != start {
            if len >= NAV_MAX_PATH_NODES {
                return false;
            }
            let parent = self.parent[current as usize];
            if parent == INVALID_NODE {
                return false;
            }
            reverse_nodes[len] = current;
            reverse_traversals[len] = self.parent_traversal[current as usize];
            len += 1;
            current = parent;
        }
        for index in 0..len {
            route.nodes[index] = reverse_nodes[len - index - 1];
            route.traversals[index] = reverse_traversals[len - index - 1];
        }
        route.len = len;
        route.cursor = 0;
        true
    }
}

fn navigation_edge_is_clear(
    graph: &ArenaNavigationGraph,
    from: NavigationNodeId,
    edge: NavigationEdge,
    arena: &ArenaDefinition,
    blockers: &NavigationBlockers,
    doors: &SplitCausewayDoorState,
) -> bool {
    let from = graph.nodes[from as usize].position;
    let to = graph.nodes[edge.to as usize].position;
    navigation_route_segment_is_clear(from, to, edge.traversal, arena, blockers, doors)
}

fn navigation_route_segment_is_clear(
    from: Vec3,
    to: Vec3,
    traversal: NavigationTraversal,
    arena: &ArenaDefinition,
    blockers: &NavigationBlockers,
    doors: &SplitCausewayDoorState,
) -> bool {
    if blockers.blocks_segment(from, to, FIGHTER_RADIUS) {
        return false;
    }
    let clearance = match traversal {
        NavigationTraversal::Walk => 0.04,
        NavigationTraversal::Jump
        | NavigationTraversal::DashJump
        | NavigationTraversal::Drop => NAV_JUMP_CLEARANCE,
    };
    let sweep_height = from.y.max(to.y) + clearance;
    navigation_segment_clear_with_doors_for_arena(
        arena,
        Vec3::new(from.x, sweep_height, from.z),
        Vec3::new(to.x, sweep_height, to.z),
        FIGHTER_RADIUS,
        doors,
    )
}

#[derive(Resource, Debug)]
pub(crate) struct BotNavigationCache {
    graphs: Vec<ArenaNavigationGraph>,
    scratch: NavigationSearchScratch,
    routes: [CachedNavigationRoute; FIGHTER_COUNT],
}

impl Default for BotNavigationCache {
    fn default() -> Self {
        let definitions = arena_definitions();
        let mut graphs = Vec::with_capacity(definitions.len());
        for arena in definitions {
            graphs.push(ArenaNavigationGraph::build(arena).unwrap_or_else(|error| {
                panic!("failed to build navigation for arena {:?}: {error}", arena.name)
            }));
        }
        Self {
            graphs,
            scratch: NavigationSearchScratch::default(),
            routes: std::array::from_fn(|_| CachedNavigationRoute::default()),
        }
    }
}

impl BotNavigationCache {
    /// Returns a deterministic normalized XZ direction toward the first node
    /// on a supported route. Search storage is reused and does not allocate.
    pub(crate) fn next_direction(
        &mut self,
        fighter_id: usize,
        decision_tick: u64,
        arena_index: usize,
        from: Vec3,
        destination: Vec3,
        blockers: &NavigationBlockers,
        doors: &SplitCausewayDoorState,
    ) -> Option<Vec2> {
        let (graphs, scratch, routes) = (&self.graphs, &mut self.scratch, &mut self.routes);
        let graph = graphs.get(arena_index)?;
        let arena = arena_definition(arena_index);
        let route = routes.get_mut(fighter_id)?;
        let start = graph.nearest_node(from)?;
        let goal = graph.nearest_node(destination)?;
        let blocker_fingerprint = blockers.fingerprint();

        for _ in 0..2 {
            if !route.matches(arena_index, goal, destination, blocker_fingerprint) {
                route.begin_plan(
                    arena_index,
                    goal,
                    destination,
                    blocker_fingerprint,
                    decision_tick,
                );
                if !scratch.build_route(graph, start, goal, arena, blockers, doors, route) {
                    route.invalidate();
                    return None;
                }
            }

            route.advance_reached_waypoints(graph, from, decision_tick);
            let (waypoint, traversal) = if route.cursor < route.len {
                (
                    graph.nodes[route.nodes[route.cursor] as usize].position,
                    route.traversals[route.cursor],
                )
            } else {
                (destination, NavigationTraversal::Walk)
            };

            if !navigation_route_segment_is_clear(
                from, waypoint, traversal, arena, blockers, doors,
            ) {
                route.invalidate();
                continue;
            }
            let flat = Vec2::new(waypoint.x - from.x, waypoint.z - from.z);
            if !route.record_progress(flat.length(), decision_tick) {
                route.invalidate();
                continue;
            }
            return Some(flat.normalize_or_zero());
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arena_defs::TRAINING_GROUND_ARENA_INDEX;

    #[test]
    fn shared_graphs_stay_within_fixed_capacities() {
        let cache = BotNavigationCache::default();
        assert_eq!(cache.graphs.len(), arena_definitions().len());
        for graph in &cache.graphs {
            assert!(graph.width <= NAV_MAX_GRID_SIDE);
            assert!(graph.depth <= NAV_MAX_GRID_SIDE);
            assert!(graph.nodes.len() <= NAV_MAX_NODES);
            assert!(graph.nodes.iter().all(|node| node
                .edges
                .iter()
                .flatten()
                .all(|edge| (edge.to as usize) < graph.nodes.len())));
        }
    }

    #[test]
    fn repeated_routes_are_deterministic_and_reuse_search_storage() {
        let arena = &arena_definitions()[TRAINING_GROUND_ARENA_INDEX];
        let mut cache = BotNavigationCache::default();
        let blockers = NavigationBlockers::default();
        let doors = SplitCausewayDoorState::default();
        let capacities = (
            cache.scratch.visited_generation.capacity(),
            cache.scratch.parent.capacity(),
            cache.scratch.parent_traversal.capacity(),
            cache.scratch.queue.capacity(),
        );
        let expected = cache
            .next_direction(
                0,
                0,
                TRAINING_GROUND_ARENA_INDEX,
                arena.spawn_points[0],
                arena.spawn_points[1],
                &blockers,
                &doors,
            )
            .expect("training spawn points should share a route");
        let revision = cache.routes[0].revision;

        for tick in 1..8 {
            assert_eq!(
                cache.next_direction(
                    0,
                    tick,
                    TRAINING_GROUND_ARENA_INDEX,
                    arena.spawn_points[0],
                    arena.spawn_points[1],
                    &blockers,
                    &doors,
                ),
                Some(expected)
            );
        }
        assert_eq!(cache.routes[0].revision, revision);
        assert_eq!(
            capacities,
            (
                cache.scratch.visited_generation.capacity(),
                cache.scratch.parent.capacity(),
                cache.scratch.parent_traversal.capacity(),
                cache.scratch.queue.capacity(),
            )
        );
    }

    #[test]
    fn authored_arena_spawns_and_supported_items_have_routes() {
        let mut cache = BotNavigationCache::default();
        let blockers = NavigationBlockers::default();
        let mut doors = SplitCausewayDoorState::default();
        // The west/east lookout item anchors are intentionally enclosed by
        // interactive doors. Exercise their reachable gameplay state here;
        // the closed state is covered by the live-door rejection test.
        doors.open_all_for_navigation_test();
        for (arena_index, arena) in arena_definitions().iter().enumerate() {
            for destination in arena.spawn_points.iter().skip(1) {
                let spawn_node = cache.graphs[arena_index].nearest_node(arena.spawn_points[0]);
                let destination_node = cache.graphs[arena_index].nearest_node(*destination);
                let route = cache.next_direction(
                    0,
                    0,
                    arena_index,
                    arena.spawn_points[0],
                    *destination,
                    &blockers,
                    &doors,
                );
                assert!(
                    route.is_some(),
                    "{} should route from spawn {:?} (node {:?}) to spawn {:?} (node {:?})",
                    arena.name,
                    arena.spawn_points[0],
                    spawn_node,
                    destination,
                    destination_node,
                );
            }
            for anchor in arena.item_anchors {
                let spawn_node = cache.graphs[arena_index].nearest_node(arena.spawn_points[0]);
                if let Some(anchor_node) =
                    cache.graphs[arena_index].nearest_node(anchor.position)
                {
                    let route = cache.next_direction(
                        0,
                        0,
                        arena_index,
                        arena.spawn_points[0],
                        anchor.position,
                        &blockers,
                        &doors,
                    );
                    assert!(
                        route.is_some(),
                        "{} should route from spawn {:?} (node {:?}) to supported item anchor {:?} (node {})",
                        arena.name,
                        arena.spawn_points[0],
                        spawn_node,
                        anchor.position,
                        anchor_node,
                    );
                }
            }
        }
    }

    #[test]
    fn dynamic_blockers_replan_or_make_the_goal_unreachable() {
        let arena = &arena_definitions()[TRAINING_GROUND_ARENA_INDEX];
        let doors = SplitCausewayDoorState::default();
        let mut cache = BotNavigationCache::default();
        let mut blockers = NavigationBlockers::default();
        let baseline = cache
            .next_direction(
                0,
                0,
                TRAINING_GROUND_ARENA_INDEX,
                arena.spawn_points[0],
                arena.spawn_points[1],
                &blockers,
                &doors,
            )
            .expect("training route should exist");
        let first_node = cache.routes[0].nodes[cache.routes[0].cursor];
        let first_waypoint = cache.graphs[TRAINING_GROUND_ARENA_INDEX].nodes[first_node as usize]
            .position;
        assert!(blockers.push(
            Vec2::new(first_waypoint.x, first_waypoint.z),
            NAV_CELL_SIZE * 0.45,
        ));
        let rerouted = cache
            .next_direction(
                0,
                1,
                TRAINING_GROUND_ARENA_INDEX,
                arena.spawn_points[0],
                arena.spawn_points[1],
                &blockers,
                &doors,
            )
            .expect("a small blocker should leave a route around it");
        assert_ne!(rerouted, baseline);

        blockers.clear();
        assert!(blockers.push(
            Vec2::new(arena.spawn_points[1].x, arena.spawn_points[1].z),
            arena.ringout_radius,
        ));
        assert_eq!(
            cache.next_direction(
                0,
                2,
                TRAINING_GROUND_ARENA_INDEX,
                arena.spawn_points[0],
                arena.spawn_points[1],
                &blockers,
                &doors,
            ),
            None
        );
    }

    #[test]
    fn closed_split_causeway_door_rejects_an_immediate_static_clear_segment() {
        let arena = &arena_definitions()[1];
        let doors = SplitCausewayDoorState::default();
        let y = arena.spawn_points[0].y + 0.04;
        let mut found_live_door = false;
        'search: for x_step in -40..=40 {
            for z_step in -40..=40 {
                let center = Vec3::new(x_step as f32 * 0.25, y, z_step as f32 * 0.25);
                for axis in [Vec3::X, Vec3::Z] {
                    let from = center - axis * 0.7;
                    let to = center + axis * 0.7;
                    if navigation_segment_clear_for_arena(arena, from, to, FIGHTER_RADIUS)
                        && !navigation_segment_clear_with_doors_for_arena(
                            arena,
                            from,
                            to,
                            FIGHTER_RADIUS,
                            &doors,
                        )
                    {
                        found_live_door = true;
                        break 'search;
                    }
                }
            }
        }
        assert!(found_live_door, "closed doors should add live collision");
    }
}
