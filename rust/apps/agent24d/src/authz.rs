//! Kernel-side decision seam; the default preserves module-private isolation.

use crate::os_memory::SpaceId;

pub enum Actor<'a> {
    #[allow(dead_code)] // The default policy only compares module and space.
    User(&'a str),
}

pub enum Op {
    #[allow(dead_code)] // Reserved by the contract; lending uses Write today.
    Read,
    Write,
    #[allow(dead_code)] // Reserved by the contract; lending uses Write today.
    Admin,
}

/// 请求级作用域。M1 恒为 None；占位以保持 architecture.md 的签名，不承载数据。
pub struct ActiveScope<'a> {
    _p: std::marker::PhantomData<&'a ()>,
}

pub struct AccessRequest<'a> {
    #[allow(dead_code)] // The default policy does not inspect the actor.
    pub actor: Actor<'a>,
    pub module: &'a str,
    pub space: &'a SpaceId,
    #[allow(dead_code)] // The default policy treats all operations equally.
    pub op: Op,
    #[allow(dead_code)] // Requests currently carry None; no scope policy yet.
    pub scope: Option<&'a ActiveScope<'a>>,
}

pub struct Decision {
    pub allow: bool,
    pub reason: &'static str,
}

pub trait Authorizer: Send + Sync {
    fn decide(&self, req: &AccessRequest<'_>) -> Decision;
}

pub struct ModulePrivateOnly;

impl Authorizer for ModulePrivateOnly {
    fn decide(&self, req: &AccessRequest<'_>) -> Decision {
        let allow = req.space == &SpaceId::module_private(req.module);
        Decision {
            allow,
            reason: if allow {
                "module may access its private space"
            } else {
                "space belongs to another module"
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AccessRequest, Actor, Authorizer, ModulePrivateOnly, Op};
    use crate::os_memory::SpaceId;

    fn request<'a>(module: &'a str, space: &'a SpaceId, op: Op) -> AccessRequest<'a> {
        AccessRequest {
            actor: Actor::User("test-user"),
            module,
            space,
            op,
            scope: None,
        }
    }

    #[test]
    fn permits_own_space_for_each_operation_with_a_reason() {
        let authorizer = ModulePrivateOnly;
        let space = SpaceId::module_private("alpha");
        for op in [Op::Read, Op::Write, Op::Admin] {
            let decision = authorizer.decide(&request("alpha", &space, op));
            assert!(decision.allow);
            assert!(!decision.reason.trim().is_empty());
        }
    }

    #[test]
    fn rejects_another_modules_space_for_each_operation_with_a_reason() {
        let authorizer = ModulePrivateOnly;
        let space = SpaceId::module_private("beta");
        for op in [Op::Read, Op::Write, Op::Admin] {
            let decision = authorizer.decide(&request("alpha", &space, op));
            assert!(!decision.allow);
            assert!(!decision.reason.trim().is_empty());
        }
    }
}
