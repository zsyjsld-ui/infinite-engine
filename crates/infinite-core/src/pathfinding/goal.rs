use super::*;
use std::{collections::VecDeque, sync::{Arc,Mutex}};

/// Retain two recent goal trees. Pending searches hold their own references,
/// so eviction cannot restart them or invalidate an in-progress path trace.
#[derive(Default)]
pub(crate) struct SharedGoals {
    fields: VecDeque<(u32, Arc<Mutex<GoalField>>)>,
}
impl SharedGoals {
    pub fn contains(&self, goal:u32)->bool { self.fields.iter().any(|(g,_)|*g==goal) }
    pub fn get(&mut self, goal:u32)->Arc<Mutex<GoalField>> {
        if let Some((_,field))=self.fields.iter().find(|(g,_)|*g==goal){return Arc::clone(field);}
        let mut field=GoalField { nodes:HashMap::default(),open:BinaryHeap::new(),limited:false };
        field.nodes.insert(goal,Node{parent:None,cost:0,closed:false});
        field.open.push(Reverse((0,goal)));
        let field=Arc::new(Mutex::new(field));
        if self.fields.len()==2{self.fields.pop_front();}
        self.fields.push_back((goal,Arc::clone(&field)));field
    }
}

pub(crate) struct GoalField {
    nodes:HashMap<u32,Node,BuildHasherDefault<CellKeyHasher>>,
    open:BinaryHeap<Reverse<(u64,u32)>>,
    limited:bool,
}
impl GoalField {
    /// Dijkstra on the same undirected, weighted collision graph as A*.
    /// All neighbours of a settled node are expanded before yielding Ready,
    /// allowing subsequent starting points to continue the shared frontier.
    pub fn reach(&mut self,map:&CollisionMap,start:u32,budget:&mut NavigationBudget)->Option<Result<(),PathError>> {
        loop {
            if self.nodes.get(&start).is_some_and(|n|n.closed){return Some(Ok(()));}
            if self.limited{return Some(Err(PathError::SearchLimit));}
            let prepared=map.navigation_cache_bytes()!=0;
            if !budget.try_charge(if prepared{16}else{64}){return None;}
            let Some(Reverse((cost,cell)))=self.open.pop()else{return Some(Err(PathError::Unreachable));};
            let node=self.nodes.get_mut(&cell).expect("queued cell");
            if node.closed||node.cost!=cost{continue;}node.closed=true;
            let(width,height)=map.nav_dimensions();let(x,y)=(i64::from(cell%width),i64::from(cell/width));
            for(direction,(dx,dy,edge))in NAV_STEPS.into_iter().enumerate(){
                let(nx,ny)=(x+dx,y+dy);
                if nx<0||ny<0||nx>=i64::from(width)||ny>=i64::from(height){continue;}
                let next=ny as u32*width+nx as u32;
                if !map.nav_edge_is_clear(cell,next,direction){continue;}
                let next_cost=cost+map.nav_edge_cost(cell,next,edge);
                if self.nodes.get(&next).is_some_and(|n|n.closed||n.cost<=next_cost){continue;}
                if (!self.nodes.contains_key(&next)&&self.nodes.len()==MAX_PATH_SEARCH_NODES)||self.open.len()==MAX_OPEN_ENTRIES{
                    self.limited=true;break;
                }
                self.nodes.insert(next,Node{parent:Some(cell),cost:next_cost,closed:false});
                self.open.push(Reverse((next_cost,next)));
            }
        }
    }
    pub fn parent(&self,cell:u32)->Option<u32>{
        let node=self.nodes.get(&cell).expect("settled trace cell");assert!(node.closed);node.parent
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU32;
    #[test]
    fn shared_distances_match_independent_dense_dijkstra_on_both_grids(){
        let n=|v|NonZeroU32::new(v).unwrap();
        for staggered in [false,true]{for prepared in [false,true]{
            let mut map=if staggered{CollisionMap::new_staggered(GridPosition{x:0,y:0},n(12),n(14),n(64),n(16)).unwrap()}
                else{CollisionMap::new(GridPosition{x:0,y:0},n(12),n(14),n(64)).unwrap()};
            for y in 0..11{map.set_blocked(5,y,true).unwrap();}if prepared{map.prepare_navigation();}
            let goal=10+2*12;let mut distance=vec![u64::MAX;168];let mut closed=vec![false;168];distance[goal as usize]=0;
            while let Some(cell)=(0..168).filter(|&c|!closed[c]&&distance[c]!=u64::MAX).min_by_key(|&c|(distance[c],c)){
                closed[cell]=true;
                for(dir,(dx,dy,edge))in NAV_STEPS.into_iter().enumerate(){let(nx,ny)=(cell as i64%12+dx,cell as i64/12+dy);if !(0..12).contains(&nx)||!(0..14).contains(&ny){continue;}
                    let next=(ny*12+nx)as usize;if map.nav_edge_is_clear(cell as u32,next as u32,dir){distance[next]=distance[next].min(distance[cell]+map.nav_edge_cost(cell as u32,next as u32,edge));}
                }
            }
            let mut goals=SharedGoals::default();let field=goals.get(goal);let mut field=field.lock().unwrap();
            for cell in 0..168{if !map.is_walkable(map.nav_center(cell)){continue;}
                let mut budget=NavigationBudget::new(u32::MAX);let result=field.reach(&map,cell,&mut budget).unwrap();
                if distance[cell as usize]==u64::MAX{assert_eq!(result,Err(PathError::Unreachable));}else{
                    result.unwrap();assert_eq!(field.nodes[&cell].cost,distance[cell as usize]);
                    let mut at=cell;let mut steps=0;while let Some(next)=field.parent(at){assert!(map.segment_is_clear(map.nav_center(at),map.nav_center(next)));assert!(field.nodes[&next].cost<field.nodes[&at].cost);at=next;steps+=1;assert!(steps<=168);}assert_eq!(at,goal);
                }
            }
        }}
    }
}
