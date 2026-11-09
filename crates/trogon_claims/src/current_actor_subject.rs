use crate::actor_subject_error::ActorSubjectError;
use crate::current_actor::CurrentActor;
use crate::object_type::ObjectType;
use crate::subject_mapping::SubjectMapping;
use crate::subject_reference::SubjectReference;

impl SubjectReference {
    pub fn try_from_current_actor<M: SubjectMapping>(
        actor: &CurrentActor,
        object_type: ObjectType,
    ) -> Result<Self, ActorSubjectError> {
        let subject = actor.key().subject().ok_or(ActorSubjectError::NoSubject)?;
        Ok(Self::new(object_type, M::map(subject)?))
    }
}
