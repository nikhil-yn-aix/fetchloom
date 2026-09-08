//! The Source implementation every endpoint-and-field provider shares.

macro_rules! delegating_source {
    ($provider:ty) => {
        impl Source for $provider {
            type Body = HttpBody;

            fn serves(&self, reference: &str) -> Option<Serves> {
                reference
                    .strip_prefix(self.scheme())
                    .and_then(|rest| rest.strip_prefix(':'))
                    .map(|_| self.serving(reference))
            }

            fn take_degradations(&self) -> Vec<Degradation> {
                self.http.take_degradations()
            }

            fn probe(
                &self,
                location: &str,
                credential: Option<&Credential>,
            ) -> Result<SourceMetadata, Error> {
                let resolved = self.located(location, credential)?;
                let started = Instant::now();
                let (answer, served) = self.http.send(Method::Head, &resolved, None, credential)?;
                let elapsed = started.elapsed();
                let status = answer.status().as_u16();
                if !(200..300).contains(&status) {
                    return Err(crate::http::status_failure(
                        &resolved,
                        status,
                        header(&answer, "retry-after").as_deref(),
                        credential,
                    ));
                }
                Ok(self.metadata(&served, &answer, elapsed, location))
            }

            fn fetch(
                &self,
                location: &str,
                range: Option<ByteRange>,
                credential: Option<&Credential>,
                resuming: Option<&str>,
            ) -> Result<Served<Self::Body>, Error> {
                let resolved = self.located(location, credential)?;
                let started = Instant::now();
                let (answer, served) =
                    self.http
                        .resuming(Method::Get, &resolved, range, credential, resuming)?;
                let elapsed = started.elapsed();
                check_fetch_status(&resolved, range, &answer, credential)?;
                let metadata = self.metadata(&served, &answer, elapsed, location);
                Ok(Served {
                    metadata,
                    body: HttpBody::new(answer.into_body().into_reader()),
                })
            }

            fn revalidate(
                &self,
                location: &str,
                validator: &Validator,
                credential: Option<&Credential>,
            ) -> Result<Revalidated<Self::Body>, Error> {
                let resolved = self.located(location, credential)?;
                let started = Instant::now();
                let (answer, served) = self.http.send_conditional(
                    Method::Get,
                    &resolved,
                    None,
                    credential,
                    validator,
                )?;
                let elapsed = started.elapsed();
                let status = answer.status().as_u16();
                if status == 304 {
                    return Ok(Revalidated::Unchanged);
                }
                if !(200..300).contains(&status) {
                    return Err(crate::http::status_failure(
                        &resolved,
                        status,
                        header(&answer, "retry-after").as_deref(),
                        credential,
                    ));
                }
                let metadata = self.metadata(&served, &answer, elapsed, location);
                Ok(Revalidated::Changed(Box::new(Served {
                    metadata,
                    body: HttpBody::new(answer.into_body().into_reader()),
                })))
            }

            fn list(
                &self,
                location: &str,
                credential: Option<&Credential>,
            ) -> Result<Listing, Error> {
                self.listed(location, credential)
            }
        }
    };
}

pub(crate) use delegating_source;
