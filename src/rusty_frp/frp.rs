use topological_sort::TopologicalSort;
use std::any::Any;
use std::collections::HashMap;
use std::collections::HashSet;
use std::marker::PhantomData;

pub struct FrpContext<ENV> {
    free_cell_id: u32,
    cell_map: HashMap<u32,CellImpl<ENV,Any>>,
    cell_loop_map: HashMap<u32,u32>,
    cells_to_be_updated: HashSet<u32>,
    change_notifiers: Vec<Box<Fn(&mut ENV)>>,

    // when executing inside cell_switch, this will hold that cell's id
    inside_cell_switch_id_op: Option<u32>,

    transaction_depth: u32
}

pub trait WithFrpContext<ENV> {
    fn with_frp_context<'r>(&self, &'r mut ENV) -> &'r mut FrpContext<ENV>;
}

impl<ENV: 'static> FrpContext<ENV> {

    pub fn new() -> FrpContext<ENV> {
        FrpContext {
            free_cell_id: 0,
            cell_map: HashMap::new(),
            cell_loop_map: HashMap::new(),
            cells_to_be_updated: HashSet::new(),
            change_notifiers: Vec::new(),
            inside_cell_switch_id_op: None,
            transaction_depth: 0
        }
    }

    pub fn cell_loop<A,F,F2>(env: &mut ENV, with_frp_context: &F, time0_value: A, k:F2) -> Cell<ENV,A>
    where
    A:'static,
    F:WithFrpContext<ENV>,
    F2:Fn(&mut ENV,&F,&Cell<ENV,A>)->Cell<ENV,A>
    {
        let cell = FrpContext::new_cell_sink(env, with_frp_context, time0_value);
        let cell2 = k(env,with_frp_context,&Cell::of(cell.id));
        {
            let frp_context = with_frp_context.with_frp_context(env);
            frp_context.cell_loop_map.insert(cell.id, cell2.id);
        }
        return Cell::of(cell2.id);
    }

    fn cell_switch<F,A,CCA>(env: &mut ENV, with_frp_context: &WithFrpContext<ENV>, cell_thunk_cell_a: &CCA) -> Cell<ENV,A>
    where
    A:'static,
    CCA:CellTrait<ENV,Box<Fn(&mut ENV,&WithFrpContext<ENV>)->Cell<ENV,A>>>
    {
        let initial_value_thunk = cell_current_value(cell_thunk_cell_a, env, with_frp_context);
        let ca: CellSink<ENV,Cell<ENV,A>> = FrpContext::new_cell_sink(env, with_frp_context, Cell::of(0));
        {
            let frp_context = with_frp_context.with_frp_context(env);
            frp_context.inside_cell_switch_id_op = Some(ca.id.clone());
        }
        let initial_value = initial_value_thunk(env, with_frp_context);
        {
            let frp_context = with_frp_context.with_frp_context(env);
            frp_context.inside_cell_switch_id_op = None;
            if let Some(cell) = frp_context.cell_map.get_mut(&cell_thunk_cell_a.id()) {
                cell.dependent_cells.push(ca.id());
            }
            if let Some(cell) = frp_context.cell_map.get_mut(&ca.id) {
                let cell_thunk_cell_a2 = Cell::of(cell_thunk_cell_a.id());
                let update_fn = move |env: &mut ENV, with_frp_context: &WithFrpContext<ENV>, result: &mut Any| {
                    let thunk: &Box<Fn(&mut ENV,&WithFrpContext<ENV>)->Cell<ENV,A>> = cell_current_value(&cell_thunk_cell_a2, env, with_frp_context);
                    // TODO: Free previous child graph here
                    match result.downcast_mut::<Cell<ENV,A>>() {
                        Some(result2) => {
                            *result2 = thunk(env, with_frp_context);
                        },
                        None => ()
                    }
                };
                cell.value = Value::AnotherCell(Cell::of(initial_value.id()));
                cell.depends_on_cells.push(cell_thunk_cell_a.id());
                cell.update_fn_op = Some(Box::new(update_fn));
            }
        }
        Cell::of(ca.id())
    }

    pub fn new_stream_sink<A,F>(env: &mut ENV, with_frp_context: &F) -> StreamSink<ENV,A>
    where
    A:'static,
    F:WithFrpContext<ENV>
    {
        let frp_context = with_frp_context.with_frp_context(env);
        let cell_id = frp_context.free_cell_id;
        frp_context.free_cell_id = frp_context.free_cell_id + 1;
        let initial_value: Option<A> = None;
        frp_context.insert_cell(
            CellImpl {
                id: cell_id,
                free_observer_id: 0,
                observer_map: HashMap::new(),
                update_fn_op: None,
                dependent_cells: Vec::new(),
                depends_on_cells: Vec::new(),
                reset_value_after_propergate_op: Some(Box::new(
                    |a| {
                        *a = None;
                    }
                )),
                child_cells: Vec::new(),
                value: Value::Direct(Box::new(initial_value) as Box<Option<A>>)
            }
        );
        StreamSink::of(cell_id)
    }

    pub fn new_cell_sink<A>(env: &mut ENV, with_frp_context: &WithFrpContext<ENV>, value: A) -> CellSink<ENV,A>
    where
    A:'static
    {
        let frp_context = with_frp_context.with_frp_context(env);
        let cell_id = frp_context.free_cell_id;
        frp_context.free_cell_id = frp_context.free_cell_id + 1;
        frp_context.insert_cell(
            CellImpl {
                id: cell_id,
                free_observer_id: 0,
                observer_map: HashMap::new(),
                update_fn_op: None,
                dependent_cells: Vec::new(),
                depends_on_cells: Vec::new(),
                reset_value_after_propergate_op: None,
                child_cells: Vec::new(),
                value: Value::Direct(Box::new(value))
            }
        );
        CellSink::of(cell_id)
    }

    pub fn map_stream<A,B,SA,F,F2>(env: &mut ENV, with_frp_context: &F, sa: &SA, f: F2) -> Stream<ENV,B>
    where
    A:'static,
    B:Any + 'static,
    SA:StreamTrait<ENV,A>,
    F:WithFrpContext<ENV>,
    F2:Fn(&A)->B + 'static
    {
        let f2 = Box::new(f);
        let c = FrpContext::map_cell(
            env,
            with_frp_context,
            &sa.as_cell(),
            move |a| {
                match a {
                    &Some(ref a2) => Some(f2(&a2)),
                    &None => None
                }
            }
        );
        let cell_id = c.id.clone();
        let frp_context = with_frp_context.with_frp_context(env);
        if let Some(cell) = frp_context.cell_map.get_mut(&cell_id) {
            cell.reset_value_after_propergate_op = Some(Box::new(|a| {
                match a.downcast_mut::<Option<A>>() {
                    Some(a2) => {
                        *a2 = None;
                    },
                    None => ()
                }
            }));
        }
        Stream::of(cell_id)
    }

    pub fn map_cell<A,B,CA,F,F2>(env: &mut ENV, with_frp_context: &F, cell: &CA, f: F2) -> Cell<ENV,B>
    where
    A:'static,
    B:Any + 'static,
    CA:CellTrait<ENV,A>,
    F:WithFrpContext<ENV>,
    F2:Fn(&A)->B + 'static
    {
        let initial_value = f(cell_current_value(cell, env, with_frp_context));
        let cell = Cell::of(cell.id().clone());
        let frp_context = with_frp_context.with_frp_context(env);
        let new_cell_id = frp_context.free_cell_id;
        frp_context.free_cell_id = frp_context.free_cell_id + 1;
        if let Some(cell_impl) = frp_context.cell_map.get_mut(&cell.id) {
            cell_impl.dependent_cells.push(new_cell_id);
        }
        let update_fn = move |env: &mut ENV, with_frp_context: &WithFrpContext<ENV>, result: &mut B| {
            let frp_context = with_frp_context.with_frp_context(env);
            *result = f(cell_current_value_via_context(&cell, frp_context));
        };
        frp_context.insert_cell(
            CellImpl {
                id: new_cell_id,
                free_observer_id: 0,
                observer_map: HashMap::new(),
                update_fn_op: Some(Box::new(update_fn)),
                dependent_cells: Vec::new(),
                depends_on_cells: vec!(cell.id.clone()),
                reset_value_after_propergate_op: None,
                child_cells: Vec::new(),
                value: Value::Direct(Box::new(initial_value))
            }
        );
        return Cell::of(new_cell_id);
    }

    pub fn lift2_cell<A,B,C,CA,CB,F,F2>(env: &mut ENV, with_frp_context: &F, f: F2, cell_a: &CA, cell_b: &CB) -> Cell<ENV,C>
    where
    A:'static,
    B:'static,
    C:'static,
    CA: CellTrait<ENV,A>,
    CB: CellTrait<ENV,B>,
    F:WithFrpContext<ENV>,
    F2:Fn(&A,&B)->C + 'static
    {
        let cell_a = Cell::of(cell_a.id().clone());
        let cell_b = Cell::of(cell_b.id().clone());
        let initial_value;
        {
            let value_a = cell_current_value(&cell_a, env, with_frp_context);
            let value_b = cell_current_value(&cell_b, env, with_frp_context);
            initial_value =
                f(
                    value_a, value_b
                );
        }
        let frp_context = with_frp_context.with_frp_context(env);
        let new_cell_id = frp_context.free_cell_id;
        frp_context.free_cell_id = frp_context.free_cell_id + 1;
        if let Some(cell_a_impl) = frp_context.cell_map.get_mut(&cell_a.id) {
            cell_a_impl.dependent_cells.push(new_cell_id);
        }
        if let Some(cell_b_impl) = frp_context.cell_map.get_mut(&cell_b.id) {
            cell_b_impl.dependent_cells.push(new_cell_id);
        }
        let update_fn = move |env: &mut ENV, with_frp_context: &WithFrpContext<ENV>, result: &mut C| {
            let frp_context = with_frp_context.with_frp_context(env);
            *result = f(
                cell_current_value_via_context(&cell_a, frp_context),
                cell_current_value_via_context(&cell_b, frp_context)
            );
        };
        frp_context.insert_cell(
            CellImpl {
                id: new_cell_id,
                free_observer_id: 0,
                observer_map: HashMap::new(),
                update_fn_op: Some(Box::new(update_fn)),
                dependent_cells: Vec::new(),
                depends_on_cells: vec!(cell_a.id.clone(), cell_b.id.clone()),
                reset_value_after_propergate_op: None,
                child_cells: Vec::new(),
                value: Value::Direct(Box::new(initial_value))
            }
        );
        return Cell::of(new_cell_id);
    }

    pub fn lift3_cell<A,B,C,D,CA,CB,CC,F,F2>(env: &mut ENV, with_frp_context: &F, f: F2, cell_a: &CA, cell_b: &CB, cell_c: &CC) -> Cell<ENV,D>
    where
    A:'static,
    B:'static,
    C:'static,
    D:'static,
    CA:CellTrait<ENV,A>,
    CB:CellTrait<ENV,B>,
    CC:CellTrait<ENV,C>,
    F:WithFrpContext<ENV>,
    F2:Fn(&A,&B,&C)->D + 'static
    {
        let cell_a = Cell::of(cell_a.id().clone());
        let cell_b = Cell::of(cell_b.id().clone());
        let cell_c = Cell::of(cell_c.id().clone());
        let initial_value;
        {
            let value_a = cell_current_value(&cell_a, env, with_frp_context);
            let value_b = cell_current_value(&cell_b, env, with_frp_context);
            let value_c = cell_current_value(&cell_c, env, with_frp_context);
            initial_value =
                f(
                    value_a, value_b, value_c
                );
        }
        let frp_context = with_frp_context.with_frp_context(env);
        let new_cell_id = frp_context.free_cell_id;
        frp_context.free_cell_id = frp_context.free_cell_id + 1;
        if let Some(cell_a_impl) = frp_context.cell_map.get_mut(&cell_a.id) {
            cell_a_impl.dependent_cells.push(new_cell_id);
        }
        if let Some(cell_b_impl) = frp_context.cell_map.get_mut(&cell_b.id) {
            cell_b_impl.dependent_cells.push(new_cell_id);
        }
        if let Some(cell_c_impl) = frp_context.cell_map.get_mut(&cell_c.id) {
            cell_c_impl.dependent_cells.push(new_cell_id);
        }
        let update_fn = move |env: &mut ENV, with_frp_context: &WithFrpContext<ENV>, result: &mut D| {
            let frp_context = with_frp_context.with_frp_context(env);
            *result = f(
                cell_current_value_via_context(&cell_a, frp_context),
                cell_current_value_via_context(&cell_b, frp_context),
                cell_current_value_via_context(&cell_c, frp_context)
            )
        };
        frp_context.insert_cell(
            CellImpl {
                id: new_cell_id,
                free_observer_id: 0,
                observer_map: HashMap::new(),
                update_fn_op: Some(Box::new(update_fn)),
                dependent_cells: Vec::new(),
                depends_on_cells: vec!(cell_a.id.clone(), cell_b.id.clone(), cell_c.id.clone()),
                reset_value_after_propergate_op: None,
                child_cells: Vec::new(),
                value: Value::Direct(Box::new(initial_value))
            }
        );
        return Cell::of(new_cell_id);
    }

    pub fn lift4_cell<A,B,C,D,E,CA,CB,CC,CD,F,F2>(env: &mut ENV, with_frp_context: &F, f: F2, cell_a: &CA, cell_b: &CB, cell_c: &CC, cell_d: &CD) -> Cell<ENV,E>
    where
    A:'static,
    B:'static,
    C:'static,
    D:'static,
    E:'static,
    CA: CellTrait<ENV,A>,
    CB: CellTrait<ENV,B>,
    CC: CellTrait<ENV,C>,
    CD: CellTrait<ENV,D>,
    F:WithFrpContext<ENV>,
    F2:Fn(&A,&B,&C,&D)->E + 'static
    {
        let cell_a = Cell::of(cell_a.id().clone());
        let cell_b = Cell::of(cell_b.id().clone());
        let cell_c = Cell::of(cell_c.id().clone());
        let cell_d = Cell::of(cell_d.id().clone());
        let initial_value;
        {
            let value_a = cell_current_value(&cell_a, env, with_frp_context);
            let value_b = cell_current_value(&cell_b, env, with_frp_context);
            let value_c = cell_current_value(&cell_c, env, with_frp_context);
            let value_d = cell_current_value(&cell_d, env, with_frp_context);
            initial_value =
                f(
                    value_a, value_b, value_c, value_d
                );
        }
        let frp_context = with_frp_context.with_frp_context(env);
        let new_cell_id = frp_context.free_cell_id;
        frp_context.free_cell_id = frp_context.free_cell_id + 1;
        if let Some(cell_a_impl) = frp_context.cell_map.get_mut(&cell_a.id) {
            cell_a_impl.dependent_cells.push(new_cell_id);
        }
        if let Some(cell_b_impl) = frp_context.cell_map.get_mut(&cell_b.id) {
            cell_b_impl.dependent_cells.push(new_cell_id);
        }
        if let Some(cell_c_impl) = frp_context.cell_map.get_mut(&cell_c.id) {
            cell_c_impl.dependent_cells.push(new_cell_id);
        }
        if let Some(cell_d_impl) = frp_context.cell_map.get_mut(&cell_d.id) {
            cell_d_impl.dependent_cells.push(new_cell_id);
        }
        let update_fn = move |env: &mut ENV, with_frp_context: &WithFrpContext<ENV>, result: &mut E| {
            let frp_context = with_frp_context.with_frp_context(env);
            *result = f(
                cell_current_value_via_context(&cell_a, frp_context),
                cell_current_value_via_context(&cell_b, frp_context),
                cell_current_value_via_context(&cell_c, frp_context),
                cell_current_value_via_context(&cell_d, frp_context)
            );
        };
        frp_context.insert_cell(
            CellImpl {
                id: new_cell_id,
                free_observer_id: 0,
                observer_map: HashMap::new(),
                update_fn_op: Some(Box::new(update_fn)),
                dependent_cells: Vec::new(),
                depends_on_cells: vec!(cell_a.id.clone(), cell_b.id.clone(), cell_c.id.clone(), cell_d.id.clone()),
                reset_value_after_propergate_op: None,
                child_cells: Vec::new(),
                value: Value::Direct(Box::new(initial_value))
            }
        );
        return Cell::of(new_cell_id);
    }

    pub fn transaction<F,F2>(env: &mut ENV, with_frp_context: &F, k: F2)
    where
    F:WithFrpContext<ENV>, F2: FnOnce(&mut ENV, &F),
    {
        {
            let frp_context = with_frp_context.with_frp_context(env);
            frp_context.transaction_depth = frp_context.transaction_depth + 1;
        }
        k(env, with_frp_context);
        let final_transaction_depth;
        {
            let frp_context = with_frp_context.with_frp_context(env);
            frp_context.transaction_depth = frp_context.transaction_depth - 1;
            final_transaction_depth = frp_context.transaction_depth;
        }
        if final_transaction_depth == 0 {
            FrpContext::propergate(env, with_frp_context);
        }
    }

    fn propergate<F>(env: &mut ENV, with_frp_context: &F)
    where F:WithFrpContext<ENV>
    {
        let mut ts = TopologicalSort::<u32>::new();
        let mut change_notifiers: Vec<Box<Fn(&mut ENV)>> = Vec::new();
        let change_notifiers2: *mut Vec<Box<Fn(&mut ENV)>> = &mut change_notifiers;

        {
            let frp_context = with_frp_context.with_frp_context(env);
            frp_context.transaction_depth = frp_context.transaction_depth + 1;
            for cell_to_be_updated in &frp_context.cells_to_be_updated {
                ts.insert(cell_to_be_updated.clone());
                if let &Some(cell) = &frp_context.cell_map.get(cell_to_be_updated) {
                    for dependent_cell in &cell.dependent_cells {
                        ts.add_dependency(cell.id, dependent_cell.clone());
                    }
                }
            }
        }
        loop {
            let next_op = ts.pop();
            match next_op {
                Some(cell_id) => {
                    FrpContext::update_cell(env, with_frp_context, &cell_id);
                },
                None => break
            }
        }
        {
            let frp_context = with_frp_context.with_frp_context(env);
            frp_context.transaction_depth = frp_context.transaction_depth - 1;
            unsafe { (*change_notifiers2).append(&mut frp_context.change_notifiers) };
        }
        for change_notifier in change_notifiers {
            change_notifier(env);
        }
        {
            let frp_context = with_frp_context.with_frp_context(env);
            let cells_to_be_updated = frp_context.cells_to_be_updated.clone();
            for cell_to_be_updated in cells_to_be_updated {
                if let Some(cell) = frp_context.cell_map.get_mut(&cell_to_be_updated) {
                    match &cell.reset_value_after_propergate_op {
                        &Some(ref reset_value_after_propergate) => {
                            reset_value_after_propergate(&mut cell.value);
                        },
                        &None => ()
                    }
                }
            }
            frp_context.cells_to_be_updated.clear();
        }
    }

    fn insert_cell<A: Any + 'static>(&mut self, cell: CellImpl<ENV,A>) {
        let cell_id = cell.id.clone();
        let cell2 = cell.into_any();
        self.cell_map.insert(cell_id, cell2);
        let inside_cell_switch_id_op = self.inside_cell_switch_id_op.clone();
        if let Some(inside_cell_switch_id) = inside_cell_switch_id_op {
            if let Some(inside_cell_switch) = self.cell_map.get_mut(&inside_cell_switch_id) {
                inside_cell_switch.child_cells.push(cell_id);
            }
        }
    }

    fn free_cell(&mut self, cell_id: &u32) {
        let mut depends_on_cells: Vec<u32> = Vec::new();
        let mut child_cells: Vec<u32> = Vec::new();
        if let Some(cell) = self.cell_map.get_mut(cell_id) {
            for depends_on_cell in &cell.depends_on_cells {
                depends_on_cells.push(depends_on_cell.clone());
            }
            child_cells.append(&mut cell.child_cells);
        }
        for depends_on_cell in depends_on_cells {
            if let Some(cell) = self.cell_map.get_mut(cell_id) {
                cell.dependent_cells.retain(|id| { id != cell_id });
            }
        }
        for child_cell in child_cells.drain(..) {
            self.cell_map.remove(&child_cell);
        }
        self.cell_map.remove(cell_id);
    }

    fn update_cell(env: &mut ENV, with_frp_context: &WithFrpContext<ENV>, cell_id: &u32)
    {
        let mut notifiers_to_add: Vec<Box<Fn(&mut ENV)>> = Vec::new();
        let mut update_fn_op: Option<*mut FnMut(&mut ENV, &WithFrpContext<ENV>, &mut Any)> = None;
        {
            let frp_context = with_frp_context.with_frp_context(env);
            if let Some(cell) = frp_context.cell_map.get_mut(cell_id) {
                match &mut cell.update_fn_op {
                    &mut Some(ref mut update_fn) => {
                        update_fn_op = Some(update_fn.as_mut());
                    },
                    &mut None => ()
                }
            }
        }
        match update_fn_op {
            Some(x) => {
                let update_fn: &mut FnMut(&mut ENV, &WithFrpContext<ENV>, &mut Any);
                update_fn = unsafe { &mut *x };
                let value: *mut Any;
                {
                    let frp_context = with_frp_context.with_frp_context(env);
                    if let Some(cell) = frp_context.cell_map.get_mut(cell_id) {
                        match &mut cell.value {
                            &mut Value::Direct(ref mut x) => {
                                value = x.as_mut();
                            },
                            &mut Value::AnotherCell(_) => return
                        }
                    } else {
                        return;
                    }
                }
                update_fn(env, with_frp_context, unsafe { &mut *value });
            },
            None => ()
        }
        let frp_context = with_frp_context.with_frp_context(env);
        if let Some(cell) = frp_context.cell_map.get_mut(cell_id) {
            let cell2: *const CellImpl<ENV,Any> = cell;
            notifiers_to_add.push(Box::new(
                move |env| {
                    unsafe {
                        let ref cell3: CellImpl<ENV,Any> = *cell2;
                        for observer in cell3.observer_map.values() {
                            match &cell3.value {
                                &Value::Direct(ref x) => {
                                    observer(env, x.as_ref());
                                },
                                &Value::AnotherCell(_) => ()
                            }
                        }
                    }
                }
            ));
        }
        frp_context.change_notifiers.append(&mut notifiers_to_add);
    }

    fn mark_all_decendent_cells_for_update(&mut self, cell_id: u32, visited: &mut HashSet<u32>) {
        self.cells_to_be_updated.insert(cell_id);
        visited.insert(cell_id);
        let mut dependent_cells: Vec<u32> = Vec::new();
        dependent_cells.push(cell_id);
        match self.cell_map.get(&cell_id) {
            Some(cell) => {
                for dependent_cell in &cell.dependent_cells {
                    dependent_cells.push(dependent_cell.clone());
                }
            },
            None => ()
        }
        loop {
            let dependent_cell_op = dependent_cells.pop();
            match dependent_cell_op {
                Some(dependent_cell) => {
                    if !visited.contains(&dependent_cell) {
                        self.mark_all_decendent_cells_for_update(dependent_cell, visited);
                    }
                },
                None => break
            }
        }
    }
}

pub trait StreamTrait<ENV:'static,A:'static>: Sized {
    fn id(&self) -> u32;

    fn as_cell(&self) -> Cell<ENV,Option<A>> {
        Cell::of(self.id())
    }

    fn observe<F,F2>(&self, env: &mut ENV, with_frp_context: &F, observer: F2) -> Box<FnOnce(&mut ENV, &F)>
    where
    F:WithFrpContext<ENV>,
    F2:Fn(&mut ENV,&A) + 'static
    {
        let observer2 = Box::new(observer);
        let c: Cell<ENV,Option<A>> = Cell::of(self.id());
        c.observe(
            env,
            with_frp_context,
            move |env, a| {
                match a {
                    &Some(ref a2) => observer2(env, &a2),
                    &None => ()
                }
            }
        )
    }
}

pub trait CellTrait<ENV:'static,A:'static>: Sized {
    fn id(&self) -> u32;

    fn current_value<'a,F>(&self, env: &'a mut ENV, with_frp_context: &F) -> &'a A
    where
    F:WithFrpContext<ENV>
    {
        cell_current_value(self, env, with_frp_context)
    }

    fn observe<F,F2>(&self, env: &mut ENV, with_frp_context: &F, observer: F2) -> Box<FnOnce(&mut ENV, &F)>
    where
    F:WithFrpContext<ENV>,
    F2:Fn(&mut ENV,&A) + 'static
    {
        {
            let env2: *mut ENV = env;
            let value = self.current_value(unsafe { &mut *env2 }, with_frp_context);
            let value2: *const A = value;
            observer(unsafe { &mut *env2 }, unsafe { &*value2 });
        }
        let mut observer_id_op: Option<u32> = None;
        let observer_id_op2: *mut Option<u32> = &mut observer_id_op;
        let cell_id = self.id().clone();
        {
            let frp_context = with_frp_context.with_frp_context(env);
            if let Some(cell) = frp_context.cell_map.get_mut(&cell_id) {
                let observer_id = cell.free_observer_id;
                unsafe { *observer_id_op2 = Some(observer_id); }
                cell.free_observer_id = cell.free_observer_id + 1;
                cell.observer_map.insert(observer_id, Box::new(
                    move |env, value| {
                        match value.downcast_ref::<A>() {
                            Some(value) => observer(env, value),
                            None => ()
                        }
                    }
                ));
            }
        }
        let cell_id = self.id().clone();
        match observer_id_op {
            Some(observer_id) => {
                return Box::new(move |env, with_frp_context| {
                    let frp_context = with_frp_context.with_frp_context(env);
                    if let Some(cell) = frp_context.cell_map.get_mut(&cell_id) {
                        cell.observer_map.remove(&observer_id);
                    }
                });
            },
            None => Box::new(|_, _| {})
        }
    }
}

// NOTE: Not safe for API use. Internal use only!
fn cell_current_value<ENV:'static,A:'static,C>(cell: &C, env: &mut ENV, with_frp_context: &WithFrpContext<ENV>) -> &'static A
where
C: CellTrait<ENV,A>
{
    let mut value_op: Option<*const A> = None;
    let value_op2: *mut Option<*const A> = &mut value_op;
    {
        let frp_context = with_frp_context.with_frp_context(env);
        let value = cell_current_value_via_context(cell, frp_context);
        unsafe { (*value_op2) = Some(value); }
    }
    match value_op {
        Some(value) => {
            unsafe { &*value }
        },
        None => panic!("")
    }
}

// NOTE: Not safe for API use. Internal use only!
fn cell_current_value_via_context<ENV:'static,A:'static,C>(cell: &C, frp_context: &FrpContext<ENV>) -> &'static A
where
C: CellTrait<ENV,A>
{
    if let Some(loop_id) = frp_context.cell_loop_map.get(&cell.id()) {
        return cell_current_value_via_context(&Cell::of(loop_id.clone()), frp_context);
    }
    let result: *const A;
    match frp_context.cell_map.get(&cell.id()) {
        Some(cell) => {
            match &cell.value {
                &Value::Direct(ref x) => {
                    match x.as_ref().downcast_ref::<A>() {
                        Some(value) => result = value,
                        None => panic!("paniced on id: {}", cell.id)
                    }
                },
                &Value::AnotherCell(ref x) => {
                    let cell2: Cell<ENV,A> = Cell::of(x.id.clone());
                    result = cell_current_value_via_context(&cell2, frp_context);
                }
            }
        },
        None => panic!("")
    }
    return unsafe { &*result };
}

pub struct Cell<ENV,A:?Sized> {
    id: u32,
    env_phantom: PhantomData<ENV>,
    value_phantom: PhantomData<A>
}

impl<ENV:'static,A:'static> Clone for Cell<ENV,A> {
    fn clone(&self) -> Self {
        Cell::of(self.id.clone())
    }
}

impl<ENV:'static,A:'static> Copy for Cell<ENV,A> {}

impl<ENV:'static,A:'static> CellTrait<ENV,A> for Cell<ENV,A> {
    fn id(&self) -> u32 {
        self.id
    }
}

impl<ENV,A:?Sized> Cell<ENV,A> {
    fn of(id: u32) -> Cell<ENV,A> {
        Cell {
            id: id,
            env_phantom: PhantomData,
            value_phantom: PhantomData
        }
    }
}

pub struct CellSink<ENV,A:?Sized> {
    id: u32,
    env_phantom: PhantomData<ENV>,
    value_phantom: PhantomData<A>
}

impl<ENV:'static,A:'static> Clone for CellSink<ENV,A> {
    fn clone(&self) -> Self {
        CellSink::of(self.id.clone())
    }
}

impl<ENV:'static,A:'static> Copy for CellSink<ENV,A> {}

impl<ENV:'static,A:'static> CellTrait<ENV,A> for CellSink<ENV,A> {
    fn id(&self) -> u32 {
        self.id
    }
}

impl<ENV:'static,A:'static> CellSink<ENV,A> {
    fn of(id: u32) -> CellSink<ENV,A> {
        CellSink {
            id: id,
            env_phantom: PhantomData,
            value_phantom: PhantomData
        }
    }

    pub fn change_value<F>(&self, env: &mut ENV, with_frp_context: &F, value: A)
    where F:WithFrpContext<ENV> {
        let cell_id = self.id.clone();
        FrpContext::transaction(
            env,
            with_frp_context,
            move |env, with_frp_context| {
                let frp_context = with_frp_context.with_frp_context(env);
                if let Some(cell) = frp_context.cell_map.get_mut(&cell_id) {
                    cell.value = Value::Direct(Box::new(value) as Box<Any>);
                }
                frp_context.mark_all_decendent_cells_for_update(cell_id, &mut HashSet::new());
            }
        );
    }
}

pub struct Stream<ENV,A> {
    id: u32,
    env_phantom: PhantomData<ENV>,
    value_phantom: PhantomData<A>
}

impl<ENV:'static,A:'static> Stream<ENV,A> {
    fn of(id: u32) -> Stream<ENV,A> {
        Stream {
            id: id,
            env_phantom: PhantomData,
            value_phantom: PhantomData
        }
    }
}

impl<ENV:'static,A:'static> StreamTrait<ENV,A> for Stream<ENV,A> {
    fn id(&self) -> u32 {
        return self.id.clone();
    }
}

pub struct StreamSink<ENV,A> {
    id: u32,
    env_phantom: PhantomData<ENV>,
    value_phantom: PhantomData<A>
}

impl<ENV:'static,A:'static> StreamSink<ENV,A> {
    fn of(id: u32) -> StreamSink<ENV,A> {
        StreamSink {
            id: id,
            env_phantom: PhantomData,
            value_phantom: PhantomData
        }
    }

    pub fn send<F>(&self, env: &mut ENV, with_frp_context: &F, value: A)
    where
    F:WithFrpContext<ENV>
    {
        CellSink::of(self.id).change_value(env, with_frp_context, Some(value));
    }
}


impl<ENV:'static,A:'static> StreamTrait<ENV,A> for StreamSink<ENV,A> {
    fn id(&self) -> u32 {
        return self.id.clone();
    }
}

struct CellImpl<ENV,A:?Sized> {
    id: u32,
    free_observer_id: u32,
    observer_map: HashMap<u32,Box<Fn(&mut ENV,&A)>>,
    update_fn_op: Option<Box<FnMut(&mut ENV, &WithFrpContext<ENV>, &mut A)>>,
    dependent_cells: Vec<u32>,
    depends_on_cells: Vec<u32>,

    reset_value_after_propergate_op: Option<Box<Fn(&mut A)>>,

    // When a cell gets freed, these child cells get freed also. It gets used in cell_switch(...).
    child_cells: Vec<u32>,

    value: Value<ENV,A>
}

enum Value<ENV,A:?Sized> {
    Direct(Box<A>),
    AnotherCell(Cell<ENV,A>)
}

impl<ENV:'static,A:?Sized> CellImpl<ENV,A> {
    fn into_any(mut self) -> CellImpl<ENV,Any>
    where A:Sized + 'static
    {
        let mut observer_map: HashMap<u32,Box<Fn(&mut ENV,&Any)>> = HashMap::new();
        for (k,v) in self.observer_map.drain() {
            observer_map.insert(k, Box::new(
                move |env, a| {
                    match a.downcast_ref::<A>() {
                        Some(a2) => v(env, a2),
                        None => ()
                    }
                }
            ));
        }
        let old_update_fn_op = self.update_fn_op;
        let mut update_fn_op: Option<Box<FnMut(&mut ENV, &WithFrpContext<ENV>, &mut Any) + 'static>>;
        match old_update_fn_op {
            Some(mut update_fn) => {
                let update_fn2 = move |env: &mut ENV, with_frp_context: &WithFrpContext<ENV>, a: &mut Any| {
                    match a.downcast_mut::<A>() {
                        Some(a2) => update_fn.as_mut()(env, with_frp_context, a2),
                        None => ()
                    }
                };
                update_fn_op = Some(Box::new(update_fn2));
            },
            None => {
                update_fn_op = None;
            }
        }
        let reset_value_after_propergate_op: Option<Box<Fn(&mut Any)>>;
        match self.reset_value_after_propergate_op {
            Some(reset_value_after_propergate) => {
                reset_value_after_propergate_op = Some(
                    Box::new(move |a: &mut Any| {
                       match a.downcast_mut::<A>() {
                           Some(a2) => {
                               reset_value_after_propergate(a2);
                           },
                           None => ()
                       };
                   })
                );
            },
            None => {
                reset_value_after_propergate_op = None;
            }
        }
        let value = match self.value {
            Value::Direct(x) => Value::Direct(x as Box<Any>),
            Value::AnotherCell(x) => {
                let cell: Cell<ENV,Any> = Cell::of(x.id);
                Value::AnotherCell(cell)
            }
        };
        CellImpl {
            id: self.id,
            free_observer_id: self.free_observer_id,
            observer_map: observer_map,
            update_fn_op: update_fn_op,
            dependent_cells: self.dependent_cells,
            depends_on_cells: self.depends_on_cells,
            reset_value_after_propergate_op: reset_value_after_propergate_op,
            child_cells: self.child_cells,
            value: value
        }
    }
}
