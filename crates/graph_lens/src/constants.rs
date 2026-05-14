//! Constants for the Graph Lens physics engine and canvas rendering.
//! Tuning these values alters the visual density, stability, and
//! performance of the force-directed layout simulation.

// ==========================================
// PHYSICS SIMULATION CONSTANTS
// ==========================================

/// Maximum allowed velocity per tick. Prevents nodes from "exploding"
/// off the screen during initial chaotic simulation ticks.
pub const MAX_VELOCITY: f32 = 100.0;
pub const VELOCITY_CLAMP: f32 = MAX_VELOCITY;

/// Simulation loop stops when the "temperature" (step size) drops below this value.
pub const MIN_TEMPERATURE: f32 = 0.01;

/// The target rest length of an edge between a parent directory and its child file.
pub const IDEAL_EDGE_LENGTH: f32 = 150.0;

/// Spring stiffness pulling connected nodes together.
pub const SPRING_STRENGTH: f32 = 0.05;

/// Barnes-Hut repulsion strength pushing all nodes apart.
pub const REPULSION_STRENGTH: f32 = 40000.0;

/// Gravity strength pulling all nodes toward the center of the viewport to prevent drifting.
pub const GRAVITY_STRENGTH: f32 = 0.01;

/// Multiplier applied to temperature each tick to gradually freeze the simulation.
pub const COOLING_FACTOR: f32 = 0.95;

/// The maximum number of physics iterations to run before forcing a stop.
pub const MAX_ITERATIONS: usize = 100;

// ==========================================
// COLLISION & OVERLAP CONSTANTS
// ==========================================

/// Distance threshold (in logical pixels) below which two nodes are considered
/// to be overlapping and need hard correction forces applied.
pub const OVERLAP_DISTANCE_THRESHOLD: f32 = 1.0;

/// Minimum squared distance used to avoid division by zero when computing overlap forces.
pub const MIN_OVERLAP_DISTANCE_SQ: f32 = 1.0;

/// Stiffness multiplier for overlap correction forces. Higher values aggressively
/// push intersecting nodes apart but can induce jitter.
pub const OVERLAP_CORRECTION_STIFFNESS: f32 = 2.0;

/// Arbitrary distance to explicitly bump nodes apart if their centers perfectly overlap.
pub const PUSH_DISTANCE: f32 = 5.0;

// ==========================================
// COMPOUND GRAPH (DIRECTORY) CONSTANTS
// ==========================================

/// Minimum width for a parent directory node, regardless of children.
pub const PARENT_MIN_WIDTH: f32 = 200.0;

/// Minimum height for a parent directory node, regardless of children.
pub const PARENT_MIN_HEIGHT: f32 = 100.0;

/// Vertical padding above child nodes within a parent directory (leaves room for the directory label/header).
pub const COMPOUND_PADDING_TOP: f32 = 60.0;

/// Horizontal and bottom padding around child nodes within a parent directory.
pub const COMPOUND_PADDING_OTHER: f32 = 10.0;

/// Linear interpolation (lerp) factor for smooth parent directory resizing during layout.
/// Values between 0.0 and 1.0; lower is smoother/slower.
pub const PARENT_SIZE_LERP_FACTOR: f32 = 0.1;

// ==========================================
// RENDERING & INTERACTION CONSTANTS
// ==========================================

/// Threshold below which a position change is considered "negligible" and
/// will be skipped when sending updates to the UI thread to save redraws.
pub const POSITION_CHANGE_THRESHOLD: f32 = 0.5;

/// Interval at which the background physics thread yields and checks for
/// UI messages (roughly 60fps).
pub const SIMULATION_POLL_INTERVAL_MS: u64 = 16;
