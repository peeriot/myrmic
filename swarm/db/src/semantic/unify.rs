//! This module just shuffles data back and forth between two representations.
//!
//! The `spargebra` crate represents user's queries.
//! And the `oxrdf` crate represents a lowered form.
//!
//! Some of these functions, if they take a `blanks` parameter,
//! replace the `BlankNode`s in the tree with "true" anonymous nodes,
//! so there's never a conflict.
//!

use oxrdf::{BlankNode, NamedNode, NamedOrBlankNode, Term, Triple};
use rustc_hash::FxHashMap;
use spareval::QuerySolution;
use spargebra::term::{
    GraphName, GraphNamePattern, GroundQuadPattern, GroundTermPattern, GroundTriplePattern,
    NamedNodePattern, QuadPattern, TermPattern, TriplePattern,
};

pub type Blanks = FxHashMap<BlankNode, BlankNode>;

fn replace_blank_node(node: BlankNode, blanks: &mut Blanks) -> BlankNode {
    blanks.entry(node).or_default().clone()
}

fn convert_named_or_blank(node: NamedOrBlankNode, blanks: &mut Blanks) -> NamedOrBlankNode {
    match node {
        NamedOrBlankNode::BlankNode(blank) => replace_blank_node(blank, blanks).into(),
        subject @ NamedOrBlankNode::NamedNode(_) => subject,
    }
}

fn convert_object(object: Term, blanks: &mut Blanks) -> Term {
    match object {
        Term::BlankNode(blank) => replace_blank_node(blank, blanks).into(),
        Term::Triple(triple) => convert_triple(*triple, blanks).into(),
        term @ (Term::NamedNode(_) | Term::Literal(_)) => term,
    }
}

pub fn convert_triple(triple: Triple, blanks: &mut Blanks) -> Triple {
    let Triple {
        subject,
        predicate,
        object,
    } = triple;

    Triple {
        subject: convert_named_or_blank(subject, blanks),
        predicate,
        object: convert_object(object, blanks),
    }
}

pub fn convert_graph_name(graph_name: GraphName) -> oxrdf::GraphName {
    match graph_name {
        GraphName::NamedNode(named_node) => oxrdf::GraphName::NamedNode(named_node),
        GraphName::DefaultGraph => oxrdf::GraphName::DefaultGraph,
    }
}

pub fn fill_quad_pattern(
    quad: &QuadPattern,
    solution: &QuerySolution,
    blanks: &mut Blanks,
) -> Option<oxrdf::Quad> {
    Some(oxrdf::Quad {
        subject: match fill_term_or_var(&quad.subject, solution, blanks)? {
            Term::NamedNode(node) => node.into(),
            Term::BlankNode(node) => node.into(),
            Term::Triple(_) | Term::Literal(_) => return None,
        },
        predicate: fill_named_node_or_var(&quad.predicate, solution)?,
        object: fill_term_or_var(&quad.object, solution, blanks)?,
        graph_name: fill_graph_name_or_var(&quad.graph_name, solution)?,
    })
}

fn fill_term_or_var(
    term: &TermPattern,
    solution: &QuerySolution,
    bnodes: &mut Blanks,
) -> Option<Term> {
    Some(match term {
        TermPattern::NamedNode(term) => term.clone().into(),
        TermPattern::BlankNode(bnode) => replace_blank_node(bnode.clone(), bnodes).into(),
        TermPattern::Literal(term) => term.clone().into(),
        TermPattern::Triple(triple) => fill_triple_pattern(triple, solution, bnodes)?.into(),
        TermPattern::Variable(v) => solution.get(v)?.clone(),
    })
}

fn fill_named_node_or_var(term: &NamedNodePattern, solution: &QuerySolution) -> Option<NamedNode> {
    Some(match term {
        NamedNodePattern::NamedNode(term) => term.clone(),
        NamedNodePattern::Variable(v) => {
            if let Term::NamedNode(s) = solution.get(v)? {
                s.clone()
            } else {
                return None;
            }
        }
    })
}

fn fill_graph_name_or_var(
    term: &GraphNamePattern,
    solution: &QuerySolution,
) -> Option<oxrdf::GraphName> {
    Some(match term {
        GraphNamePattern::NamedNode(term) => term.clone().into(),
        GraphNamePattern::DefaultGraph => oxrdf::GraphName::DefaultGraph,
        GraphNamePattern::Variable(v) => match solution.get(v)? {
            Term::NamedNode(node) => node.clone().into(),
            Term::BlankNode(node) => node.clone().into(),
            Term::Triple(_) | Term::Literal(_) => return None,
        },
    })
}

fn fill_triple_pattern(
    triple: &TriplePattern,
    solution: &QuerySolution,
    bnodes: &mut Blanks,
) -> Option<Triple> {
    Some(Triple {
        subject: match fill_term_or_var(&triple.subject, solution, bnodes)? {
            Term::NamedNode(node) => node.into(),
            Term::BlankNode(node) => node.into(),
            Term::Triple(_) | Term::Literal(_) => return None,
        },
        predicate: fill_named_node_or_var(&triple.predicate, solution)?,
        object: fill_term_or_var(&triple.object, solution, bnodes)?,
    })
}

pub fn fill_ground_quad_pattern(
    quad: &GroundQuadPattern,
    solution: &QuerySolution,
) -> Option<oxrdf::Quad> {
    Some(oxrdf::Quad {
        subject: match fill_ground_term_or_var(&quad.subject, solution)? {
            Term::NamedNode(node) => node.into(),
            Term::BlankNode(node) => node.into(),
            Term::Triple(_) | Term::Literal(_) => return None,
        },
        predicate: fill_named_node_or_var(&quad.predicate, solution)?,
        object: fill_ground_term_or_var(&quad.object, solution)?,
        graph_name: fill_graph_name_or_var(&quad.graph_name, solution)?,
    })
}

fn fill_ground_term_or_var(term: &GroundTermPattern, solution: &QuerySolution) -> Option<Term> {
    Some(match term {
        GroundTermPattern::NamedNode(term) => term.clone().into(),
        GroundTermPattern::Literal(term) => term.clone().into(),
        GroundTermPattern::Triple(triple) => fill_ground_triple_pattern(triple, solution)?.into(),
        GroundTermPattern::Variable(v) => solution.get(v)?.clone(),
    })
}

fn fill_ground_triple_pattern(
    triple: &GroundTriplePattern,
    solution: &QuerySolution,
) -> Option<Triple> {
    Some(Triple {
        subject: match fill_ground_term_or_var(&triple.subject, solution)? {
            Term::NamedNode(node) => node.into(),
            Term::BlankNode(node) => node.into(),
            Term::Triple(_) | Term::Literal(_) => return None,
        },
        predicate: fill_named_node_or_var(&triple.predicate, solution)?,
        object: fill_ground_term_or_var(&triple.object, solution)?,
    })
}
