//! The fixed provider help records, and the function that chooses one for an
//! endpoint.

use fetchloom_engine::credential::{Necessity, ProviderHelp, token_variable};
use fetchloom_engine::reference::Host;

fn google_cloud_storage(host: &str, necessity: Necessity) -> ProviderHelp {
    let variable = token_variable(&Host::new(host));
    ProviderHelp {
        provider: "Google Cloud Storage".to_owned(),
        unlocks: "Reading a bucket that is not open to everyone. Without a token, only \
            buckets whose contents are public can be fetched."
            .to_owned(),
        necessity,
        steps: vec![
            "Install the Google Cloud CLI from https://cloud.google.com/sdk/docs/install \
                by following the instructions there for your operating system. It is a \
                program called `gcloud` that you run in a terminal."
                .to_owned(),
            "Open a terminal. On Windows that is the program called Windows PowerShell or \
                Terminal. On Linux it is the program called Terminal."
                .to_owned(),
            "Type `gcloud auth login` and press Enter. A web browser opens. Sign in with \
                the Google account that can read the bucket, and choose Allow."
                .to_owned(),
            "Back in the terminal, type `gcloud auth print-access-token` and press Enter. \
                It prints one long line of letters, numbers and dashes. That line is the \
                token. Select it and copy it."
                .to_owned(),
            "Put the token where Fetchloom will find it, using the Placement steps below."
                .to_owned(),
        ],
        placement: format!(
            "An environment variable named `{variable}` holding the token, or an entry in \
                this machine's credential store under `fetchloom:{host}`. On Windows, to \
                use the credential store, open Windows PowerShell and type \
                `cmdkey /generic:fetchloom:{host} /user:fetchloom /pass:` followed by the \
                token, then press Enter. On Linux, to use the credential store, create a \
                file at `~/.config/fetchloom/credentials` containing the line \
                `{host} = \"the token you copied\"`, then type \
                `chmod 600 ~/.config/fetchloom/credentials` and press Enter. Fetchloom \
                refuses to read that file if any other user on the machine can read it."
        ),
        verification: format!(
            "`fetchloom plan https://{host}/<your bucket>/` \
                Running it reads the bucket listing and moves no bytes. If the token \
                works, the plan prints. If it does not, the failure names the token and \
                how to renew it."
        ),
        scope: "The narrowest that works is read access to the one bucket, granted by the \
            role Storage Object Viewer on that bucket. Do not grant a project-wide role."
            .to_owned(),
    }
}

fn azure_blob_storage(host: &str, necessity: Necessity) -> ProviderHelp {
    let variable = token_variable(&Host::new(host));
    ProviderHelp {
        provider: "Azure Blob Storage".to_owned(),
        unlocks: "Reading a container that is not open to everyone. Without a token, only \
            containers whose access level is set to public can be fetched."
            .to_owned(),
        necessity,
        steps: vec![
            "Install the Azure CLI from \
                https://learn.microsoft.com/cli/azure/install-azure-cli by following the \
                instructions there for your operating system. It is a program called `az` \
                that you run in a terminal."
                .to_owned(),
            "Open a terminal. On Windows that is the program called Windows PowerShell or \
                Terminal. On Linux it is the program called Terminal."
                .to_owned(),
            "Type `az login` and press Enter. A web browser opens. Sign in with the \
                account that can read the container."
                .to_owned(),
            "Back in the terminal, type \
                `az account get-access-token --resource https://storage.azure.com/ \
                --query accessToken --output tsv` and press Enter. It prints one long \
                line. That line is the token. Select it and copy it."
                .to_owned(),
            "Put the token where Fetchloom will find it, using the Placement steps below."
                .to_owned(),
        ],
        placement: format!(
            "An environment variable named `{variable}`, or an entry in this machine's \
                credential store under `fetchloom:{host}`. On Windows, open Windows \
                PowerShell and type \
                `cmdkey /generic:fetchloom:{host} /user:fetchloom /pass:` followed by the \
                token, then press Enter. On Linux, create a file at \
                `~/.config/fetchloom/credentials` containing the line \
                `{host} = \"the token you copied\"`, then type \
                `chmod 600 ~/.config/fetchloom/credentials` and press Enter."
        ),
        verification: format!(
            "`fetchloom plan https://{host}/<your container>/` Running it reads the \
                container listing and moves no bytes."
        ),
        scope: "The narrowest that works is the role Storage Blob Data Reader, assigned \
            on the one container rather than on the storage account or the subscription."
            .to_owned(),
    }
}

fn amazon_s3(host: &str, necessity: Necessity) -> ProviderHelp {
    ProviderHelp {
        provider: "Amazon S3".to_owned(),
        unlocks: "Nothing this build can use. Amazon S3 authenticates a request by \
            signing it with an access key and a secret key, which is not a token that \
            can be sent, and this build sends a token. A private Amazon S3 bucket cannot \
            be fetched by name here."
            .to_owned(),
        necessity,
        steps: vec![
            format!(
                "If the bucket is public, no token is needed. Fetch it by its endpoint, \
                    for example `https://{host}/<your bucket>/<your prefix>/`."
            ),
            "If the bucket is private, ask whoever owns it for a presigned link to each \
                object you need. A presigned link is an ordinary web address that already \
                carries its own authorization and expires after a set time."
                .to_owned(),
            format!(
                "Fetch each presigned link directly, for example \
                    `fetchloom get \"https://{host}/bucket/object?X-Amz-Signature=...\"`. \
                    Quote the address, because it contains characters a terminal would \
                    otherwise read as instructions. Fetchloom never writes the part after \
                    the question mark to any file, log, event or message."
            ),
        ],
        placement: "None. There is nowhere to put an access key and secret key that this \
            build would use."
            .to_owned(),
        verification: format!(
            "`fetchloom plan https://{host}/<your bucket>/<your prefix>/` For a public \
                bucket the plan prints. For a private one it fails naming the status the \
                bucket returned."
        ),
        scope: "For a presigned link, the narrowest that works is one object, for the \
            shortest time that covers the transfer."
            .to_owned(),
    }
}

fn generic_object_storage(host: &str, necessity: Necessity) -> ProviderHelp {
    let variable = token_variable(&Host::new(host));
    ProviderHelp {
        provider: "Object storage".to_owned(),
        unlocks: "Reading a bucket or prefix that is not open to everyone.".to_owned(),
        necessity,
        steps: vec![
            "Ask whoever runs the storage endpoint for a bearer token that can read the \
                bucket you want. A bearer token is one line of letters and numbers that a \
                program sends with every request to prove it is allowed to read."
                .to_owned(),
            "If they offer an access key and a secret key instead of a token, this build \
                cannot use them. Ask instead for a presigned link to each object you \
                need, which is an ordinary web address that carries its own \
                authorization."
                .to_owned(),
            "Put the token where Fetchloom will find it, using the Placement steps below."
                .to_owned(),
        ],
        placement: format!(
            "An environment variable named `{variable}`, or an entry in this machine's \
                credential store under `fetchloom:{host}`. On Windows you may instead \
                open Windows PowerShell and type \
                `cmdkey /generic:fetchloom:{host} /user:fetchloom /pass:` followed by the \
                token, then press Enter. On Linux you may instead create a file at \
                `~/.config/fetchloom/credentials` containing the line \
                `{host} = \"the token\"`, then type \
                `chmod 600 ~/.config/fetchloom/credentials` and press Enter. Fetchloom \
                refuses to read that file if any other user on the machine can read it."
        ),
        verification: format!(
            "`fetchloom plan https://{host}/<your bucket>/` Running it reads the listing \
                and moves no bytes."
        ),
        scope: "The narrowest that works is read access to the one bucket or prefix you \
            name, and no write access of any kind."
            .to_owned(),
    }
}

/// Chooses the fixed provider help record for an endpoint's host.
///
/// The record is selected by the host's suffix, matched case-insensitively, and carries
/// the given necessity. A host matching none of the known suffixes gets the generic
/// object storage record.
#[must_use]
pub fn help_for(host: &str, necessity: Necessity) -> ProviderHelp {
    let lowercase = host.to_ascii_lowercase();
    if lowercase.ends_with("storage.googleapis.com") {
        google_cloud_storage(host, necessity)
    } else if lowercase.ends_with("blob.core.windows.net") {
        azure_blob_storage(host, necessity)
    } else if lowercase.ends_with("amazonaws.com") {
        amazon_s3(host, necessity)
    } else {
        generic_object_storage(host, necessity)
    }
}
