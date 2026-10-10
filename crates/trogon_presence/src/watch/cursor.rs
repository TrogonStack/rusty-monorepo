use crate::position::{DiffSequence, EpochOrder, GenerationEpoch, LocalViewId, PositionOverflow, ViewIncarnation};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ViewCursor {
    Local {
        view: LocalViewId,
        incarnation: ViewIncarnation,
        seq: DiffSequence,
    },
    Service {
        epoch: GenerationEpoch,
        seq: DiffSequence,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CursorStep {
    Next,
    Repeat,
    Gap,
    Stale,
    Rebase,
}

impl ViewCursor {
    pub fn local(view: LocalViewId, incarnation: ViewIncarnation) -> Self {
        Self::Local {
            view,
            incarnation,
            seq: DiffSequence::FIRST,
        }
    }

    pub fn service(epoch: GenerationEpoch) -> Self {
        Self::Service {
            epoch,
            seq: DiffSequence::FIRST,
        }
    }

    pub fn seq(&self) -> DiffSequence {
        match self {
            Self::Local { seq, .. } | Self::Service { seq, .. } => *seq,
        }
    }

    pub fn advance(self) -> Result<Self, PositionOverflow> {
        Ok(match self {
            Self::Local { view, incarnation, seq } => Self::Local {
                view,
                incarnation,
                seq: seq.next()?,
            },
            Self::Service { epoch, seq } => Self::Service {
                epoch,
                seq: seq.next()?,
            },
        })
    }

    pub fn follow(&self, next: &Self, prev: DiffSequence) -> CursorStep {
        match self.origin_order(next) {
            EpochOrder::Older => return CursorStep::Rebase,
            EpochOrder::Newer => return CursorStep::Stale,
            EpochOrder::Conflict => return CursorStep::Rebase,
            EpochOrder::Same => {}
        }
        let (seen, offered) = (self.seq(), next.seq());
        if offered == seen && prev == seen {
            CursorStep::Repeat
        } else if offered <= seen {
            CursorStep::Stale
        } else if prev == seen {
            CursorStep::Next
        } else {
            CursorStep::Gap
        }
    }

    fn origin_order(&self, next: &Self) -> EpochOrder {
        match (self, next) {
            (
                Self::Local {
                    view: seen_view,
                    incarnation: seen,
                    ..
                },
                Self::Local { view, incarnation, .. },
            ) if seen_view == view => match seen.cmp(incarnation) {
                std::cmp::Ordering::Less => EpochOrder::Older,
                std::cmp::Ordering::Equal => EpochOrder::Same,
                std::cmp::Ordering::Greater => EpochOrder::Newer,
            },
            (Self::Service { epoch: seen, .. }, Self::Service { epoch, .. }) => {
                seen.compare(*epoch).unwrap_or(EpochOrder::Conflict)
            }
            _ => EpochOrder::Conflict,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::position::{OwnerEpoch, OwnerId, StreamGeneration};
    use crate::revision::EntryRevision;

    fn epoch(generation: u8, acquired: u64, owner: u8) -> GenerationEpoch {
        GenerationEpoch::new(
            StreamGeneration::from([generation; 16]),
            OwnerEpoch::new(EntryRevision::from(acquired), OwnerId::from([owner; 16])),
        )
    }

    #[test]
    fn service_cursor_starts_at_one_and_follows_its_chain() -> Result<(), PositionOverflow> {
        let start = ViewCursor::service(epoch(1, 10, 1));
        assert_eq!(start.seq(), DiffSequence::FIRST);
        let next = start.advance()?;
        assert_eq!(start.follow(&next, start.seq()), CursorStep::Next);
        assert_eq!(next.follow(&next, next.seq()), CursorStep::Repeat);
        let skipped = next.advance()?.advance()?;
        assert_eq!(next.follow(&skipped, DiffSequence::from(3)), CursorStep::Gap);
        assert_eq!(next.follow(&start, DiffSequence::from(0)), CursorStep::Stale);
        Ok(())
    }

    #[test]
    fn new_owner_epoch_restarts_at_one_and_forces_a_rebase() -> Result<(), PositionOverflow> {
        let old = ViewCursor::service(epoch(1, 10, 1)).advance()?.advance()?;
        let taken = ViewCursor::service(epoch(1, 20, 2));
        assert_eq!(taken.seq(), DiffSequence::FIRST);
        assert_eq!(old.follow(&taken.advance()?, taken.seq()), CursorStep::Rebase);
        assert_eq!(taken.follow(&old, DiffSequence::from(2)), CursorStep::Stale);
        let other_generation = ViewCursor::service(epoch(2, 10, 1));
        assert_eq!(old.follow(&other_generation, DiffSequence::FIRST), CursorStep::Rebase);
        Ok(())
    }

    #[test]
    fn local_incarnation_restarts_the_sequence() -> Result<(), PositionOverflow> {
        let view = LocalViewId::from([4; 16]);
        let first = ViewCursor::local(view, ViewIncarnation::FIRST).advance()?;
        let rebuilt = ViewCursor::local(view, ViewIncarnation::FIRST.next()?);
        assert_eq!(rebuilt.seq(), DiffSequence::FIRST);
        assert_eq!(first.follow(&rebuilt, DiffSequence::FIRST), CursorStep::Rebase);
        let other = ViewCursor::local(LocalViewId::from([5; 16]), ViewIncarnation::FIRST);
        assert_eq!(first.follow(&other, DiffSequence::FIRST), CursorStep::Rebase);
        Ok(())
    }
}
