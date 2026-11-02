use crate::*;

impl ListRequest {
    fn binding(&self) -> Self {
        let mut request = self.clone();
        request.cursor = None;
        request
    }
    pub fn validate_cursor(&self) -> Result<(), FileError> {
        if self
            .cursor
            .as_ref()
            .is_some_and(|cursor| *cursor.binding != self.binding())
        {
            return Err(FileError::new(
                FileErrorKind::InvalidInput,
                Operation::List,
                MutationOutcome::NotApplied,
            ));
        }
        Ok(())
    }
}
impl ListCursor {
    /// Provider-issued token bound to scope, parent, query, prefix and page size.
    pub fn new(request: &ListRequest, opaque: String) -> Result<Self, FileError> {
        request.validate_cursor()?;
        if opaque.is_empty()
            || request
                .cursor
                .as_ref()
                .is_some_and(|cursor| cursor.opaque == opaque)
        {
            return Err(FileError::new(
                FileErrorKind::InvalidInput,
                Operation::List,
                MutationOutcome::NotApplied,
            ));
        }
        Ok(Self {
            binding: Box::new(request.binding()),
            opaque,
        })
    }
    pub fn opaque(&self) -> &str {
        &self.opaque
    }
}
impl Page {
    pub fn new(
        request: &ListRequest,
        entries: Vec<Entry>,
        completeness: Completeness,
    ) -> Result<Self, FileError> {
        request.validate_cursor()?;
        if entries.len() > request.limit.get()
            || entries
                .iter()
                .any(|entry| entry.id.scope() != request.parent.scope())
            || matches!(&completeness, Completeness::More(cursor) if *cursor.binding != request.binding() || request.cursor.as_ref() == Some(cursor))
        {
            return Err(FileError::new(
                FileErrorKind::InvalidInput,
                Operation::List,
                MutationOutcome::NotApplied,
            ));
        }
        Ok(Self {
            entries,
            completeness,
        })
    }
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }
    pub fn completeness(&self) -> &Completeness {
        &self.completeness
    }
    /// A continuation exists iff completeness is More; impossible contradictory state.
    pub fn next(&self) -> Option<&ListCursor> {
        match &self.completeness {
            Completeness::More(cursor) => Some(cursor),
            _ => None,
        }
    }
    pub fn into_parts(self) -> (Vec<Entry>, Completeness) {
        (self.entries, self.completeness)
    }
}
