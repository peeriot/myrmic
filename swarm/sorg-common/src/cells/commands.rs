use cell_protocol::CellCommandError;

/// Outcome of a command called on a cell. Note that the result here relates only to the successful
/// transport and execution of the command, as well as the successful retrieval of the payload provided by the commanded cell.
/// It is the responsibility of the cell authors to agree on a representation of domain/application-level
/// errors in the paylod which is optionally delivered in the OK variant of the cell query outcome.
pub type CellCommandOutcome = Result<(), CellCommandError>;
